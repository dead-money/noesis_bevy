//! Bevy-app-level integration test for the **typed `ItemsSource`** bridge
//! (`dm_noesis_bevy::items`), exercised end-to-end through the real
//! `NoesisPlugin` pipeline (headless, pipelined rendering on).
//!
//! A [`NoesisItems`] populates a `ListBox` with **`i32`** items `[10, 20, 30]`
//! and drives its selection to index `1`. The bridge boxes each item with
//! Noesis's `push_i32` and reads the current item back out of the engine through
//! an `ICollectionView`'s typed `CurrentItem::as_i32` accessor, surfacing it (with
//! the control's `count` + `selected_index`) as a [`NoesisItemsCurrent`] message.
//!
//! Bluff-resistance — every assertion observes a value the engine round-trip
//! provably produces and that the *un-driven default* does not:
//!
//!   * `count == 3` — the typed items reached the live `ItemsControl` (a no-op
//!     apply / unbound collection reads `0`).
//!   * `selected_index == 1` — the control's selection was driven (the default
//!     for an untouched `ListBox` is `-1`, "nothing selected").
//!   * `current == ItemValue::I32(20)` — the *typed* item at the selected index,
//!     read back unboxed from Noesis. This pins down all of: the items are real
//!     boxed `i32`s (a string list unboxes to `None` / `Str`, never `I32(20)`);
//!     selection moved off the default position `0` (whose value is `10`, the
//!     negative control); and the value matches the authored list.
//!
//! Theme-free / font-free XAML: a bare `<ListBox>` with no `ItemTemplate` needs
//! no glyph rendering for the data round-trip we assert (item containers default
//! to a `TextBlock`, but we never read pixels), so the scene builds with no font
//! gate.
//!
//!   `cargo test -p dm_noesis_bevy --test headless_app_typed_items -- --nocapture`

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::{
    ItemValue, NoesisCamera, NoesisItems, NoesisItemsCurrent, NoesisPlugin, NoesisView,
    XamlRegistry,
};

const VIEW_W: u32 = 120;
const VIEW_H: u32 = 80;

/// Set the typed items + selection only after the scene exists, so the
/// component's one-shot change-detection apply isn't lost (mirrors the
/// write-only bridge tests).
const SET_AT_FRAME: usize = 12;
const EXIT_AT_FRAME: usize = 60;

const XAML: &str = r##"<ListBox xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      x:Name="Ports" Width="120" Height="80"/>"##;

/// Observations of `(name, count, selected_index, current)` per view, with the
/// frame they were seen on, so we can assert nothing arrived before the drive.
type Observed = Vec<(usize, Entity, String, usize, i32, Option<ItemValue>)>;

#[test]
fn typed_items_populate_select_and_read_back() {
    noesis_license_from_env();

    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
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

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert("ports.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "ports.xaml".to_string(),
                        size: UVec2::new(VIEW_W, VIEW_H),
                        ..default()
                    },
                    // Empty to start (no-op); filled in after the scene exists so
                    // the one-shot apply lands on a live control.
                    NoesisItems::new(),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let observed_sys = Arc::clone(&observed);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut q: Query<&mut NoesisItems>,
              mut changes: MessageReader<NoesisItemsCurrent>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for mut items in &mut q {
                    *items = NoesisItems::new()
                        .with("Ports", [10, 20, 30]) // typed i32 items
                        .select("Ports", 1); // drive selection to index 1
                }
            }

            for ev in changes.read() {
                observed_sys.lock().unwrap().push((
                    *frame,
                    ev.view,
                    ev.name.clone(),
                    ev.count,
                    ev.selected_index,
                    ev.current.clone(),
                ));
            }

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed NoesisItemsCurrent ---");
    for (f, e, name, count, idx, current) in &got {
        eprintln!("  f{f} {e:?} {name} count={count} idx={idx} current={current:?}");
    }

    // The latest snapshot seen for our control on our view.
    let latest = got
        .iter()
        .rfind(|(_, e, name, _, _, _)| *e == view && name == "Ports");
    let (_, _, _, count, idx, current) = latest
        .expect("no NoesisItemsCurrent for the ListBox — typed items never bound / read back");

    assert_eq!(
        *count, 3,
        "the 3 typed i32 items should have reached the live ListBox (got count {count})",
    );
    assert_eq!(
        *idx, 1,
        "selection should have been driven to index 1 (default for an untouched \
         ListBox is -1); got {idx}",
    );
    assert_eq!(
        *current,
        Some(ItemValue::I32(20)),
        "the current item at index 1 should read back as the typed i32 20 \
         (default position 0 is 10; a string list would never unbox to I32); got {current:?}",
    );

    // Bluff-catch: a selected_index of 1 must never have surfaced before we drove
    // it (would mean the bridge fabricated selection rather than applying ours).
    assert!(
        got.iter()
            .all(|(f, _, _, _, sel, _)| *sel != 1 || *f >= SET_AT_FRAME),
        "selected_index 1 surfaced before the drive frame; got {got:?}",
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
