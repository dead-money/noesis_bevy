//! Headless smoke test for the `examples/databinding.rs` port of the Noesis SDK
//! **`DataBinding`** ("Solar System") sample. It boots the *same* XAML the example
//! ships (shared via `include_str!`), pumps real `NoesisPlugin` frames, mutates a
//! bound value, and asserts the bound control reflects it — through the crate's
//! safe data bridges, never raw FFI.
//!
//! Two bridges, two assertions:
//!
//!   * **Binding bridge** (`#[derive(NoesisViewModel)]` plain VM as the view-root
//!     `DataContext`). The `<TextBlock Text="{Binding name}"/>` detail panel is
//!     observed via a [`NoesisText`] watch. We seed the VM to `"Sun"` (Rust→UI),
//!     then *mutate the component* to `"Mars"` mid-run and require the watch to
//!     report both — proving a live mutation of the bound value reaches the
//!     control. Bluff-resistance: `"Mars"` must never surface before the edit
//!     frame, and the seed `"Sun"` differs from both the empty default and the
//!     post-edit value.
//!   * **List/`ItemsSource` bridge** ([`NoesisItems`]). The body names populate
//!     the `ListBox`'s `ItemsSource`; we require the engine's read-back
//!     ([`NoesisItemsCurrent`]) to report the authored `count` (an unbound list
//!     reads `0`).
//!
//! Font-free assertion path: only DP/text *values* are read (no glyph pixels),
//! so the scene builds with no font gate.
//!
//!   `cargo test -p dm_noesis_bevy --test headless_databinding -- --nocapture`

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::{
    NoesisCamera, NoesisItems, NoesisItemsCurrent, NoesisPlugin, NoesisText, NoesisTextChanged,
    NoesisView, NoesisViewModel, NoesisViewModelAppExt, XamlRegistry,
};

/// The exact scene `examples/databinding.rs` ships — shared so the test can
/// never drift from the example it covers.
const XAML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/databinding/solar_system.xaml"
));

const VIEW_W: u32 = 640;
const VIEW_H: u32 = 400;

/// Mirrors the example's `PlanetVm`. Field names match the XAML `{Binding ...}`
/// targets; the derive maps them to reflected properties.
#[derive(Component, NoesisViewModel, Default)]
struct PlanetVm {
    name: String,
    orbit: f32,
    diameter: f32,
    details: String,
}

const NAMES: [&str; 4] = ["Sun", "Mercury", "Venus", "Mars"];
const SEED_NAME: &str = "Sun";
const SEED_DETAILS: &str = "The yellow dwarf star at the center of our solar system.";
const EDIT_NAME: &str = "Mars";
const EDIT_DETAILS: &str = "The red planet.";

const EDIT_AT_FRAME: usize = 18;
const EXIT_AT_FRAME: usize = 60;

type TextObs = Vec<(usize, Entity, String)>; // (frame, view, text)

#[test]
fn databinding_example_round_trips_bound_value() {
    noesis_license_from_env();

    let names_seen: Arc<Mutex<TextObs>> = Arc::new(Mutex::new(Vec::new()));
    let details_seen: Arc<Mutex<TextObs>> = Arc::new(Mutex::new(Vec::new()));
    let item_counts: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
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
    app.add_noesis_view_model::<PlanetVm>();

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "solar_system.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "solar_system.xaml".to_string(),
                        size: UVec2::new(VIEW_W, VIEW_H),
                        ..default()
                    },
                    PlanetVm {
                        name: SEED_NAME.into(),
                        orbit: 0.0,
                        diameter: 1_380_000.0,
                        details: SEED_DETAILS.into(),
                    },
                    NoesisItems::new()
                        .with("Planets", NAMES)
                        .select("Planets", 0),
                    NoesisText::new().watching(["NameText", "DetailsText"]),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let names_sys = Arc::clone(&names_seen);
    let details_sys = Arc::clone(&details_seen);
    let counts_sys = Arc::clone(&item_counts);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut vms: Query<&mut PlanetVm>,
              mut text_changes: MessageReader<NoesisTextChanged>,
              mut item_changes: MessageReader<NoesisItemsCurrent>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            for ev in text_changes.read() {
                // NoesisTextChanged carries the element name + text; route by name.
                match ev.name.as_str() {
                    "NameText" => {
                        names_sys
                            .lock()
                            .unwrap()
                            .push((*frame, ev.view, ev.text.clone()))
                    }
                    "DetailsText" => {
                        details_sys
                            .lock()
                            .unwrap()
                            .push((*frame, ev.view, ev.text.clone()))
                    }
                    _ => {}
                }
            }
            for ev in item_changes.read() {
                if ev.name == "Planets" {
                    counts_sys.lock().unwrap().push(ev.count);
                }
            }

            // Mutate the bound value mid-run: re-point the VM at Mars. The
            // plain-VM bridge re-snapshots the changed component into the bound
            // detail panel.
            if *frame == EDIT_AT_FRAME {
                for mut vm in &mut vms {
                    vm.name = EDIT_NAME.into();
                    vm.details = EDIT_DETAILS.into();
                    vm.orbit = 1.52;
                    vm.diameter = 6_794.0;
                }
            }

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let view = view_entity.lock().unwrap().expect("view spawned");
    let names = names_seen.lock().unwrap().clone();
    let details = details_seen.lock().unwrap().clone();
    let counts = item_counts.lock().unwrap().clone();

    eprintln!("--- NameText observations ---");
    for (f, e, t) in &names {
        eprintln!("  f{f} {e:?} = {t:?}");
    }
    eprintln!("--- DetailsText observations ---");
    for (f, e, t) in &details {
        eprintln!("  f{f} {e:?} = {t:?}");
    }
    eprintln!("--- Planets item counts: {counts:?}");

    let name_for = |t: &str| names.iter().any(|(_, e, v)| *e == view && v == t);
    let details_for = |t: &str| details.iter().any(|(_, e, v)| *e == view && v == t);

    // List bridge: the authored body names reached the live ListBox.
    assert!(
        counts.iter().any(|c| *c == NAMES.len()),
        "ListBox.ItemsSource never reported the {} authored bodies (an unbound \
         list reads 0); counts {counts:?}",
        NAMES.len(),
    );

    // Binding bridge — Rust→UI seed: the seeded body reached the bound panel.
    assert!(
        name_for(SEED_NAME),
        "seed {SEED_NAME:?} never reached NameText via {{Binding name}}; got {names:?}",
    );
    assert!(
        details_for(SEED_DETAILS),
        "seed details never reached DetailsText; got {details:?}",
    );

    // Binding bridge — live mutation: the mid-run edit reached the bound panel.
    assert!(
        name_for(EDIT_NAME),
        "mutated value {EDIT_NAME:?} never reached NameText; the bound control did \
         not reflect the component mutation. got {names:?}",
    );
    assert!(
        details_for(EDIT_DETAILS),
        "mutated details never reached DetailsText; got {details:?}",
    );

    // Bluff-catch: the mutated value must not surface before we wrote it.
    assert!(
        names
            .iter()
            .all(|(f, _, v)| v != EDIT_NAME || *f >= EDIT_AT_FRAME),
        "{EDIT_NAME:?} surfaced before the edit frame {EDIT_AT_FRAME}; got {names:?}",
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
