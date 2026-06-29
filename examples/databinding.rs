//! Bevy-idiomatic port of the Noesis SDK **`DataBinding`** sample
//! (`$NOESIS_SDK_DIR/Src/Packages/Samples/DataBinding`, the "Solar System"
//! tutorial).
//!
//! The SDK original binds an `ObservableCollection<SolarSystemObject>` to a
//! `ListBox` and shows the focused body's `Name`/`Orbit`/`Diameter`/`Details`.
//! This port keeps the same data flow but drives it through the crate's safe
//! data bridges — no raw FFI:
//!
//!   * [`NoesisItems`] populates the `ListBox`'s `ItemsSource` with the body
//!     names (the list/`ItemsSource` bridge) and drives its selection.
//!   * A `#[derive(NoesisViewModel)]` plain struct ([`PlanetVm`]) is attached as
//!     the view-root `DataContext`; the detail panel binds `{Binding name}` etc.
//!     to it (the binding bridge).
//!
//! Idiomatic Bevy wiring: the body table lives in a Rust [`SolarSystem`]
//! resource, a timer system advances the `ListBox` selection, and a second
//! system reacts to the engine's selection read-back ([`NoesisItemsCurrent`]) by
//! copying the now-focused body into the [`PlanetVm`] component. Because the
//! plain-VM bridge re-snapshots a changed component, the bound detail panel
//! updates automatically — selection → read-back → VM → UI, one direction.
//!
//! Run it (windowed):
//!
//! ```sh
//! cargo run -p dm_noesis_bevy --example databinding
//! ```
//!
//! The headless data round-trip is asserted by
//! `tests/headless_databinding.rs`, which shares the same XAML.

use std::sync::Arc;
use std::time::Duration;

use bevy::prelude::*;
use dm_noesis_bevy::{
    FontRegistry, NoesisCamera, NoesisItems, NoesisItemsCurrent, NoesisPlugin, NoesisView,
    NoesisViewModel, NoesisViewModelAppExt, XamlRegistry,
};

/// Shared with the smoke test so the scene under test never drifts from the one
/// the example ships.
const XAML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/databinding/solar_system.xaml"
));

const VIEW_W: u32 = 640;
const VIEW_H: u32 = 400;

/// A solar-system body. The Rust-side model the example owns and binds from.
#[derive(Clone)]
struct Body {
    name: &'static str,
    orbit: f32,
    diameter: f32,
    details: &'static str,
}

/// The body table (subset of the SDK sample's collection).
#[derive(Resource)]
struct SolarSystem {
    bodies: Vec<Body>,
}

impl Default for SolarSystem {
    fn default() -> Self {
        Self {
            bodies: vec![
                Body {
                    name: "Sun",
                    orbit: 0.0,
                    diameter: 1_380_000.0,
                    details: "The yellow dwarf star at the center of our solar system.",
                },
                Body {
                    name: "Mercury",
                    orbit: 0.38,
                    diameter: 4_880.0,
                    details: "The small, rocky planet closest to the Sun.",
                },
                Body {
                    name: "Venus",
                    orbit: 0.72,
                    diameter: 12_103.6,
                    details: "At first glance, if Earth had a twin, it would be Venus.",
                },
                Body {
                    name: "Earth",
                    orbit: 1.0,
                    diameter: 12_756.3,
                    details: "Our home planet, the only one known to harbor life.",
                },
                Body {
                    name: "Mars",
                    orbit: 1.52,
                    diameter: 6_794.0,
                    details: "The red planet has inspired flights of imagination for centuries.",
                },
                Body {
                    name: "Jupiter",
                    orbit: 5.20,
                    diameter: 142_984.0,
                    details: "With its moons and rings, Jupiter is a mini-solar-system.",
                },
                Body {
                    name: "Saturn",
                    orbit: 9.54,
                    diameter: 120_536.0,
                    details: "The most distant of the five planets known to the ancients.",
                },
                Body {
                    name: "Neptune",
                    orbit: 30.06,
                    diameter: 49_532.0,
                    details: "The first planet located through mathematical prediction.",
                },
            ],
        }
    }
}

/// Plain Bevy component bound to the view-root `DataContext` by field name. The
/// derive supplies the `NoesisViewModel` glue; field names match the XAML's
/// `{Binding name}` / `{Binding orbit}` / ... targets.
#[derive(Component, NoesisViewModel, Default)]
struct PlanetVm {
    name: String,
    orbit: f32,
    diameter: f32,
    details: String,
}

impl PlanetVm {
    fn from_body(b: &Body) -> Self {
        Self {
            name: b.name.to_string(),
            orbit: b.orbit,
            diameter: b.diameter,
            details: b.details.to_string(),
        }
    }
}

/// Ticks the `ListBox` selection forward once per interval.
#[derive(Resource)]
struct CycleTimer(Timer);

fn main() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "dm_noesis_bevy — DataBinding (Solar System)".into(),
                resolution: (VIEW_W, VIEW_H).into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(NoesisPlugin::default())
        .add_noesis_view_model::<PlanetVm>()
        .init_resource::<SolarSystem>()
        .insert_resource(CycleTimer(Timer::new(
            Duration::from_millis(1500),
            TimerMode::Repeating,
        )))
        .add_systems(Startup, setup)
        .add_systems(Update, (advance_selection, focus_from_selection))
        .run();
}

fn setup(
    mut commands: Commands,
    mut xaml: ResMut<XamlRegistry>,
    mut fonts: ResMut<FontRegistry>,
    system: Res<SolarSystem>,
) {
    xaml.insert(
        "solar_system.xaml".to_string(),
        Arc::new(XAML.as_bytes().to_vec()),
    );

    // Best-effort: register Roboto so the FontFamily="Fonts/#Roboto" reference
    // resolves and the text renders crisply. Absent, Noesis falls through to its
    // fallback chain — the scene still boots and binds.
    let font_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/Fonts/Roboto-Regular.ttf"
    );
    let have_font = match std::fs::read(font_path) {
        Ok(bytes) => {
            fonts.insert("Fonts", "Roboto-Regular.ttf", Arc::new(bytes));
            true
        }
        Err(err) => {
            warn!("Roboto font not found ({err}); falling back to default fonts");
            false
        }
    };

    let names: Vec<&str> = system.bodies.iter().map(|b| b.name).collect();

    commands.spawn((
        Camera2d,
        NoesisCamera,
        NoesisView {
            xaml_uri: "solar_system.xaml".to_string(),
            size: UVec2::new(VIEW_W, VIEW_H),
            ppaa: true,
            // Only gate on the font when we actually staged it, or the view
            // would wait forever for a file that never arrives.
            wait_for_font_files: if have_font {
                vec![("Fonts".to_string(), "Roboto-Regular.ttf".to_string())]
            } else {
                Vec::new()
            },
            ..default()
        },
        // Start focused on the first body; the timer cycles from here.
        PlanetVm::from_body(&system.bodies[0]),
        NoesisItems::new()
            .with("Planets", names)
            .select("Planets", 0),
    ));
}

/// Advance the `ListBox` selection one body forward each interval (write-only
/// drive through the items bridge).
#[allow(clippy::needless_pass_by_value)]
fn advance_selection(
    time: Res<Time>,
    mut timer: ResMut<CycleTimer>,
    system: Res<SolarSystem>,
    mut next: Local<usize>,
    mut q: Query<&mut NoesisItems>,
) {
    if !timer.0.tick(time.delta()).just_finished() {
        return;
    }
    *next = (*next + 1) % system.bodies.len();
    let index = i32::try_from(*next).unwrap_or(0);
    let names: Vec<&str> = system.bodies.iter().map(|b| b.name).collect();
    for mut items in &mut q {
        // Re-supply the names so the source isn't pruned; only the selection
        // moves. (The items bridge keys its binding by control name, so the
        // backing collection is reused — this just re-drives selection.)
        *items = NoesisItems::new()
            .with("Planets", names.clone())
            .select("Planets", index);
    }
}

/// React to the engine's selection read-back: copy the now-focused body into the
/// bound [`PlanetVm`]. The plain-VM bridge then snapshots the changed component
/// into the detail panel automatically.
fn focus_from_selection(
    mut changes: MessageReader<NoesisItemsCurrent>,
    system: Res<SolarSystem>,
    mut vms: Query<&mut PlanetVm>,
) {
    for ev in changes.read() {
        if ev.name != "Planets" {
            continue;
        }
        let Ok(index) = usize::try_from(ev.selected_index) else {
            continue; // -1 / nothing selected
        };
        let Some(body) = system.bodies.get(index) else {
            continue;
        };
        if let Ok(mut vm) = vms.get_mut(ev.view) {
            *vm = PlanetVm::from_body(body);
            info!("focused {} (orbit {} AU)", body.name, body.orbit);
        }
    }
}
