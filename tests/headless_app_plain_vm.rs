//! Bevy-app-level integration test for the **per-entity** plain-struct view
//! model bridge (`#[derive(Component, NoesisViewModel)]` +
//! `add_noesis_view_model::<T>()`), exercised end-to-end through the real
//! `NoesisPlugin` pipeline (headless, pipelined rendering on).
//!
//! It couples the new component bridge with two existing read-back paths so the
//! assertion is bluff-*resistant* in both directions:
//!
//!   * **Rust → UI.** The view entity carries a `DemoVm` component (a plain
//!     struct) with `title = "Hello"`. A `<TextBox Text="{Binding title}"/>`
//!     binds to it, and a [`NoesisText`] watch observes that box. The snapshot
//!     path must push `title` into the bound control for the watch to ever
//!     report `"Hello"`.
//!   * **UI → Rust.** A [`NoesisDp`] write sets the box's `Text` to `"World"`
//!     (simulating a user edit); the `TwoWay`/`PropertyChanged` binding pushes
//!     that into the plain-VM source, whose `on_set` writeback must flow back
//!     into the *ECS component* via the reconcile system. We assert the
//!     `DemoVm` component itself ends at `"World"`.
//!
//! A broken snapshot (Rust→UI never fires), a broken writeback drain (the
//! component never updates), or an unregistered reconcile system each fail this.
//!
//! Scope: this is a **single-view** round-trip. Two-view per-entity *routing*
//! isolation is covered by `headless_app_bridges.rs` (which exercises the same
//! `render_state` per-entity keying via the DO-backed `NoesisVm`). It can't be
//! re-tested here with the *same* plain-VM type on two views: Noesis registers a
//! reflected plain-VM class **globally by type name**, so a second instance of
//! the same `#[derive(NoesisViewModel)]` type collides at registration — a
//! Noesis-level constraint, not a keying gap.
//!
//! Font-free XAML (only DP/text values are asserted, no glyph rendering), so the
//! scene builds with no font gate.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::{
    NoesisCamera, NoesisDp, NoesisPlugin, NoesisText, NoesisTextChanged, NoesisView,
    NoesisViewModel, NoesisViewModelAppExt, XamlRegistry,
};

const SEED: &str = "Hello";
const EDIT: &str = "World";
const EDIT_AT_FRAME: usize = 14;
const EXIT_AT_FRAME: usize = 48;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <TextBox x:Name="Box"
           Text="{Binding title, Mode=TwoWay, UpdateSourceTrigger=PropertyChanged}"/>
</Grid>"##;

/// A plain Bevy component bound to XAML by field name. The derive provides the
/// `NoesisViewModel` glue; the bridge attaches it as the view-root `DataContext`.
#[derive(Component, NoesisViewModel)]
struct DemoVm {
    title: String,
}

#[test]
fn plain_vm_component_round_trips_two_way() {
    noesis_license_from_env();

    // (view entity, latest `title` snapshot of its DemoVm component).
    let titles: Arc<Mutex<HashMap<Entity, String>>> = Arc::new(Mutex::new(HashMap::new()));
    // (view entity, Box text) reported by the NoesisText watch (Rust→UI proof).
    let text_changes: Arc<Mutex<Vec<(Entity, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

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
    app.add_noesis_view_model::<DemoVm>();

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert("vm.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "vm.xaml".to_string(),
                        size: UVec2::new(64, 32),
                        ..default()
                    },
                    DemoVm { title: SEED.into() },
                    NoesisText::new().watching(["Box"]),
                    NoesisDp::new(),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let titles_sys = Arc::clone(&titles);
    let text_sys = Arc::clone(&text_changes);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              vms: Query<(Entity, &DemoVm)>,
              mut dps: Query<&mut NoesisDp>,
              mut changes: MessageReader<NoesisTextChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            // Record every view's current component title (UI→Rust readback).
            {
                let mut snap = titles_sys.lock().unwrap();
                for (e, vm) in &vms {
                    snap.insert(e, vm.title.clone());
                }
            }
            // Record Box text changes (Rust→UI readback through the watch).
            for ev in changes.read() {
                text_sys
                    .lock()
                    .unwrap()
                    .push((ev.view, ev.text.clone()));
            }

            // Simulate a user edit: drive the bound TextBox's Text via the DP
            // bridge. The TwoWay/PropertyChanged binding pushes it to the plain
            // VM, whose writeback must land back in the DemoVm component.
            if *frame == EDIT_AT_FRAME {
                for mut dp in &mut dps {
                    *dp = NoesisDp::new().set_string("Box", "Text", EDIT);
                }
            }

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let view = view_entity.lock().unwrap().expect("view spawned");
    let final_titles = titles.lock().unwrap().clone();
    let texts = text_changes.lock().unwrap().clone();

    // Rust → UI: the seeded title reached the bound TextBox (observed via watch).
    assert!(
        texts.iter().any(|(e, t)| *e == view && t == SEED),
        "Rust→UI snapshot never reached the bound TextBox; got text changes {texts:?}",
    );

    // UI → Rust: the simulated edit flowed back into the ECS component.
    assert_eq!(
        final_titles.get(&view).map(String::as_str),
        Some(EDIT),
        "UI→Rust writeback never reached the DemoVm component; titles {final_titles:?}",
    );
}

fn noesis_license_from_env() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
}
