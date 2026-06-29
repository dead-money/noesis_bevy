//! Bevy-app-level integration test for the **per-entity** bridges, exercised
//! end-to-end through the real `NoesisPlugin` pipeline (headless, pipelined
//! rendering on).
//!
//! It deliberately couples three bridges so the assertion is bluff-*resistant*:
//!
//!   1. A [`NoesisVm`] (DO-backed view model) declares a `String` property `Foo`
//!      and is attached as the view-root `DataContext`.
//!   2. A `<TextBlock x:Name="Echo" Text="{Binding Foo}"/>` binds its text to it.
//!   3. A [`NoesisText`] watch on the same view observes `Echo`'s text.
//!
//! When a system writes `Foo = "magic-smoke"` into the VM, the *only* way the
//! watch can report that exact string back via a [`NoesisTextChanged`] carrying
//! the right `view` entity is if: the VM was built, attached as `DataContext`,
//! the binding resolved, and the per-entity message path tagged the correct
//! entity. A broken attach / wrong-entity tag / missing reconcile all fail it.
//!
//! Font-free XAML (no glyph rendering is asserted — only DP values), so the
//! scene builds with no font gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::classes::PropType;
use noesis_bevy::viewmodel::{NoesisVm, ViewModelDef};
use noesis_bevy::{
    NoesisCamera, NoesisPlugin, NoesisText, NoesisTextChanged, NoesisView, XamlRegistry,
};

const SET_AT_FRAME: usize = 6;
const EXIT_AT_FRAME: usize = 40;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <TextBlock x:Name="Echo" Text="{Binding Foo}"/>
</Grid>"##;

/// Bluff-catch follow-up: **two** views, each with its own `NoesisVm` bound to
/// its own `Echo` `TextBlock` and its own `NoesisText` watch. Writing a distinct
/// sentinel into each view's VM must surface a `NoesisTextChanged` tagged with
/// the *matching* view entity — and crucially, NO message may carry one view's
/// entity with the other view's sentinel. A "route everything to the first
/// scene" bug (the failure mode single-view tests can't see) fails this.
#[test]
fn per_entity_routing_is_isolated_across_two_views() {
    noesis_license_from_env();

    let collected: Arc<Mutex<Vec<(Entity, String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let views: Arc<Mutex<Vec<(Entity, &'static str)>>> = Arc::new(Mutex::new(Vec::new()));

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .build()
            .disable::<bevy::winit::WinitPlugin>()
            .set(WindowPlugin {
                primary_window: None,
                exit_condition: ExitCondition::DontExit,
                close_when_requested: false,
                ..default()
            }),
    );
    app.add_plugins(ScheduleRunnerPlugin::run_loop(Duration::from_millis(4)));
    app.add_plugins(NoesisPlugin::default());

    let views_startup = Arc::clone(&views);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert("a.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            reg.insert("b.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            // Distinct VM class names — Noesis class registration is global by name.
            let a = spawn_view(&mut commands, "a.xaml", "RouteVmA");
            let b = spawn_view(&mut commands, "b.xaml", "RouteVmB");
            let mut v = views_startup.lock().unwrap();
            v.push((a, "alpha-A"));
            v.push((b, "bravo-B"));
        },
    );

    let collected_sys = Arc::clone(&collected);
    let views_sys = Arc::clone(&views);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut vms: Query<(Entity, &mut NoesisVm)>,
              mut changes: MessageReader<NoesisTextChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;
            if *frame == SET_AT_FRAME {
                let want = views_sys.lock().unwrap().clone();
                for (entity, mut vm) in &mut vms {
                    if let Some((_, sentinel)) = want.iter().find(|(e, _)| *e == entity) {
                        vm.set_string("Foo", *sentinel);
                    }
                }
            }
            for ev in changes.read() {
                collected_sys
                    .lock()
                    .unwrap()
                    .push((ev.view, ev.name.clone(), ev.text.clone()));
            }
            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let want = views.lock().unwrap().clone();
    let got = collected.lock().unwrap().clone();
    assert_eq!(want.len(), 2, "expected two views");
    for (entity, sentinel) in &want {
        assert!(
            got.iter()
                .any(|(e, name, text)| e == entity && name == "Echo" && text == sentinel),
            "view {entity:?} should report its own sentinel {sentinel:?}; got {got:?}",
        );
    }
    // No cross-talk: a view's entity must never carry the OTHER view's sentinel.
    let sentinels: Vec<&str> = want.iter().map(|(_, s)| *s).collect();
    for (e, _name, text) in &got {
        if let Some((_, own)) = want.iter().find(|(ve, _)| ve == e) {
            assert!(
                text != sentinels.iter().find(|s| **s != *own).unwrap(),
                "cross-talk: view {e:?} reported the other view's sentinel {text:?}",
            );
        }
    }
}

fn spawn_view(commands: &mut Commands, uri: &str, vm_class: &str) -> Entity {
    commands
        .spawn((
            Camera2d,
            NoesisCamera,
            NoesisView {
                xaml_uri: uri.to_string(),
                size: UVec2::new(64, 32),
                ..default()
            },
            NoesisVm::new(ViewModelDef::new(vm_class).property("Foo", PropType::String)),
            NoesisText::new().watching(["Echo"]),
        ))
        .id()
}

fn noesis_license_from_env() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
}
