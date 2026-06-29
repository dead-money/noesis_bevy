//! Headless smoke test for the `examples/scoreboard.rs` faithful port of the
//! Noesis SDK **Scoreboard** sample. It `#[path]`-includes the example, stages
//! the SDK's *real* `MainWindow.xaml` + fonts at runtime (never vendored), boots
//! the same wiring under the headless `ScheduleRunnerPlugin`, pumps real
//! `NoesisPlugin` frames, and asserts the scene builds + renders without panic
//! while bound values round-trip through the crate's safe bridges.
//!
//! Two assertions, both through bridges (never raw FFI):
//!
//!   1. **Object `ItemsSource` bridge** ([`NoesisItems::with_objects`]). The ten
//!      bindable player objects reach the live `Players` `ItemsControl`: the
//!      engine read-back ([`NoesisItemsCurrent`]) reports `count == 10` (an
//!      unbound list reads `0`).
//!   2. **`DataContext` binding bridge** ([`NoesisVm`] + [`NoesisDp`]). The
//!      `VisibleTeam` `ComboBox`'s `SelectedIndex="{Binding SelectedTeam}"` binds
//!      to the Game view model. We mutate `Game.SelectedTeam` mid-run (0 -> 2)
//!      and require [`NoesisDp`] to read the named combo's `SelectedIndex` back as
//!      `2` — and *never* as `2` before the edit frame (bluff-catch). That is a
//!      real bound value reaching a named element, read back via `NoesisDp`.
//!
//! When `$NOESIS_SDK_DIR` is unset the SDK assets can't be loaded, so the test
//! skips (passes) — the same graceful no-op the example uses.
//!
//!   `cargo test -p dm_noesis_bevy --test headless_example_scoreboard -- --nocapture`

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::{
    DpValue, FontRegistry, NoesisDpChanged, NoesisItemsCurrent, NoesisVm, XamlRegistry,
};

// The example is a binary; included as a module here only some items are used.
#[allow(dead_code)]
#[path = "../examples/scoreboard.rs"]
mod scoreboard;

use scoreboard::{PLAYERS_NAME, VISIBLE_TEAM_NAME};

const EDIT_AT_FRAME: usize = 30;
const EXIT_AT_FRAME: usize = 90;
const EDIT_SELECTED_TEAM: i32 = 2; // "Horde"

#[test]
fn scoreboard_example_binds_players_and_selected_team() {
    noesis_license_from_env();

    if scoreboard::sample_data_dir().is_none() {
        eprintln!("NOESIS_SDK_DIR unset — skipping scoreboard smoke test");
        return;
    }

    // (frame, count) for the Players ItemsControl read-back.
    let player_counts: Arc<Mutex<Vec<(usize, usize)>>> = Arc::new(Mutex::new(Vec::new()));
    // (frame, selected_index) read back from VisibleTeam.SelectedIndex via DP.
    let selected_idx: Arc<Mutex<Vec<(usize, i32)>>> = Arc::new(Mutex::new(Vec::new()));
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
    app.add_plugins(dm_noesis_bevy::NoesisPlugin::default());
    // The sample's `<Window>` root needs the content-host stand-in to parse.
    app.add_plugins(dm_noesis_bevy::NoesisWindowCompatPlugin);

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands,
              mut xaml: ResMut<XamlRegistry>,
              mut fonts: ResMut<FontRegistry>| {
            assert!(
                scoreboard::stage_assets(&mut xaml, &mut fonts),
                "stage_assets failed despite NOESIS_SDK_DIR being set",
            );
            let view = scoreboard::spawn_scoreboard(&mut commands);
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let counts_sys = Arc::clone(&player_counts);
    let idx_sys = Arc::clone(&selected_idx);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut vms: Query<&mut NoesisVm>,
              mut items: MessageReader<NoesisItemsCurrent>,
              mut dps: MessageReader<NoesisDpChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            for ev in items.read() {
                if ev.name == PLAYERS_NAME {
                    counts_sys.lock().unwrap().push((*frame, ev.count));
                }
            }
            for ev in dps.read() {
                if ev.name == VISIBLE_TEAM_NAME
                    && ev.property == "SelectedIndex"
                    && let DpValue::I32(v) = ev.value
                {
                    idx_sys.lock().unwrap().push((*frame, v));
                }
            }

            // Mutate the bound Game.SelectedTeam mid-run (0 -> 2); the combo's
            // {Binding SelectedTeam} must push it into SelectedIndex.
            if *frame == EDIT_AT_FRAME {
                for mut vm in &mut vms {
                    vm.set_i32("SelectedTeam", EDIT_SELECTED_TEAM);
                }
            }

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let counts = player_counts.lock().unwrap().clone();
    let indices = selected_idx.lock().unwrap().clone();
    eprintln!("--- Players counts: {counts:?}");
    eprintln!("--- VisibleTeam.SelectedIndex: {indices:?}");

    // 1. Object ItemsSource bridge: the ten bindable players reached the control.
    assert!(
        counts.iter().any(|(_, c)| *c == 10),
        "Players ItemsControl never reported the 10 bindable player objects (an \
         unbound list reads 0); counts {counts:?}",
    );

    // 2. DataContext binding bridge: the mutated SelectedTeam reached the named
    //    combo's SelectedIndex via {Binding SelectedTeam}, read back via NoesisDp.
    assert!(
        indices.iter().any(|(_, v)| *v == EDIT_SELECTED_TEAM),
        "VisibleTeam.SelectedIndex never reached {EDIT_SELECTED_TEAM} after the \
         Game.SelectedTeam edit; bound value did not round-trip. got {indices:?}",
    );
    // Bluff-catch: SelectedIndex must not have been the edited value beforehand.
    assert!(
        indices
            .iter()
            .all(|(f, v)| *v != EDIT_SELECTED_TEAM || *f >= EDIT_AT_FRAME),
        "SelectedIndex == {EDIT_SELECTED_TEAM} surfaced before the edit frame \
         {EDIT_AT_FRAME}; got {indices:?}",
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
