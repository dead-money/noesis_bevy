//! Integration tests for per-entity bridges through the real `NoesisPlugin` pipeline (headless).
//!
//! Couples three bridges per view: a [`NoesisVm`] sets a `Foo` string property as `DataContext`,
//! a `TextBlock` binds to it, and a [`NoesisText`] watch reports the result. All three must
//! work correctly for the right entity to receive the right [`NoesisTextChanged`] message.
//!
//! Font-free XAML; no glyph rendering is asserted.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::classes::PropType;
use noesis_bevy::viewmodel::{NoesisVm, ViewModelDef};
use noesis_bevy::{NoesisCamera, NoesisText, NoesisTextChanged, NoesisView, XamlRegistry};

use crate::common::{headless_app, run_until};

const SET_AT_FRAME: usize = 6;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <TextBlock x:Name="Echo" Text="{Binding Foo}"/>
</Grid>"##;

// Two views, each with its own VM/binding/watch. Catches "route to first scene"
// cross-entity routing bugs that a single-view test cannot detect.
#[test]
fn per_entity_routing_is_isolated_across_two_views() {
    let collected: Arc<Mutex<Vec<(Entity, String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let views: Arc<Mutex<Vec<(Entity, &'static str)>>> = Arc::new(Mutex::new(Vec::new()));

    let mut app = headless_app();

    let views_startup = Arc::clone(&views);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert("a.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            reg.insert("b.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            // Distinct VM class names: Noesis class registration is global by name.
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
              mut changes: MessageReader<NoesisTextChanged>| {
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
        },
    );

    // Event-driven exit: stop once both views have echoed their own sentinel.
    let pred_collected = Arc::clone(&collected);
    let pred_views = Arc::clone(&views);
    let routed = run_until(&mut app, 240, move |_app| {
        let want = pred_views.lock().unwrap();
        if want.len() < 2 {
            return false;
        }
        let got = pred_collected.lock().unwrap();
        want.iter().all(|(entity, sentinel)| {
            got.iter()
                .any(|(e, name, text)| e == entity && name == "Echo" && text == sentinel)
        })
    });

    let want = views.lock().unwrap().clone();
    let got = collected.lock().unwrap().clone();
    assert!(
        routed,
        "both views never echoed their own sentinel within 240 frames; got {got:?}",
    );
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
