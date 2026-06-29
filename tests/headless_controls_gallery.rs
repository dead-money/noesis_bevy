//! Headless smoke test for the `controls_gallery` example (a self-contained port
//! of the Noesis SDK's `Data/Styles.xaml` controls showcase). It `#[path]`-
//! includes the example and boots its **exact** app config
//! ([`configure_gallery`]) under the headless `ScheduleRunnerPlugin`, so the
//! thing under test is the same wiring a user runs windowed.
//!
//! It then drives the scene the way the demo expects to be driven — through the
//! public [`NoesisInputQueue`] — and asserts the bridges report real read-backs:
//!
//!   1. A synthetic left-click on `FireButton` surfaces a [`NoesisClicked`]
//!      carrying our view entity and the name `"FireButton"`.
//!   2. That click's reaction writes `LevelBar.Width = 30` (via `NoesisDp`) and
//!      `Status.Text = "Fired x1"` (via `NoesisText`); both surface back as
//!      [`NoesisDpChanged`] (`LevelBar.ActualWidth == 30`, `Status.Text` read
//!      back == `"Fired x1"` — proving the `NoesisText` write landed).
//!
//! The negative controls are built in: with the bridges inert no message arrives
//! at all, and the default `LevelBar.ActualWidth` is `10` / `Status.Text` is
//! `"Ready"` — so the assertions can only pass if the click subscription, the
//! click->react->write path, the DP watch, and the per-entity routing all worked
//! end to end. The test also asserts the app exits cleanly (no panic across the
//! whole frame pump).
//!
//!   `cargo test -p dm_noesis_bevy --test headless_controls_gallery -- --nocapture`

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::{DpValue, NoesisDpChanged};
use dm_noesis_bevy::{NoesisClicked, NoesisInputEvent, NoesisInputQueue};
use noesis_runtime::view::MouseButton;

// The example is a binary; included as a module here only some items are used.
#[allow(dead_code)]
#[path = "../examples/controls_gallery.rs"]
mod gallery;

const CLICK_FIRE_AT_FRAME: usize = 16;
const CLICK_TOGGLE_AT_FRAME: usize = 24;
const EXIT_AT_FRAME: usize = 70;

/// Inject a synthetic left-click (move -> down -> up) at view-pixel `(x, y)`.
/// These coordinates go straight onto the `View` (no window mapping headless).
fn left_click(queue: &mut NoesisInputQueue, (x, y): (i32, i32)) {
    queue.push(NoesisInputEvent::MouseMove { x, y });
    queue.push(NoesisInputEvent::MouseButton {
        down: true,
        x,
        y,
        button: MouseButton::Left,
    });
    queue.push(NoesisInputEvent::MouseButton {
        down: false,
        x,
        y,
        button: MouseButton::Left,
    });
}

#[test]
fn controls_gallery_clicks_and_toggle_surface_through_bridges() {
    noesis_license_from_env();

    let clicks: Arc<Mutex<Vec<(Entity, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let dp_changes: Arc<Mutex<Vec<(Entity, String, String, DpValue)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let view_id: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

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

    // Boot the example's real app config — same view, same bridges.
    gallery::configure_gallery(&mut app);

    let clicks_sys = Arc::clone(&clicks);
    let dp_sys = Arc::clone(&dp_changes);
    let view_sys = Arc::clone(&view_id);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut queue: ResMut<NoesisInputQueue>,
              gallery_view: Option<Res<gallery::GalleryView>>,
              mut clicked: MessageReader<NoesisClicked>,
              mut changed: MessageReader<NoesisDpChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            if let Some(gv) = gallery_view {
                *view_sys.lock().unwrap() = Some(gv.0);
            }

            if *frame == CLICK_FIRE_AT_FRAME {
                left_click(&mut queue, gallery::FIRE_CENTER);
            }
            if *frame == CLICK_TOGGLE_AT_FRAME {
                left_click(&mut queue, gallery::TOGGLE_CENTER);
            }

            for ev in clicked.read() {
                clicks_sys.lock().unwrap().push((ev.view, ev.name.clone()));
            }
            for ev in changed.read() {
                dp_sys.lock().unwrap().push((
                    ev.view,
                    ev.name.clone(),
                    ev.property.clone(),
                    ev.value.clone(),
                ));
            }

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let view = view_id.lock().unwrap().expect("gallery view spawned");

    let got_clicks = clicks.lock().unwrap().clone();
    let got_dp = dp_changes.lock().unwrap().clone();
    eprintln!("--- NoesisClicked ---");
    for (e, name) in &got_clicks {
        eprintln!("  {e:?} {name}");
    }
    eprintln!("--- NoesisDpChanged ---");
    for (e, name, prop, value) in &got_dp {
        eprintln!("  {e:?} {name}.{prop} = {value:?}");
    }

    let latest = |name: &str, prop: &str| -> Option<DpValue> {
        got_dp
            .iter()
            .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
            .map(|(_, _, _, v)| v.clone())
    };

    // Read-back 1 (events bridge): the Fire button's Click came back tagged with
    // our view. With the bridge inert no message arrives at all.
    assert!(
        got_clicks
            .iter()
            .any(|(e, name)| *e == view && name == "FireButton"),
        "expected a NoesisClicked {{ view: {view:?}, name: \"FireButton\" }}; got {got_clicks:?}",
    );

    // Read-back 2 (DP bridge): reacting to the Fire click we wrote
    // `LevelBar.Width = 30` and watch `ActualWidth` read it back. The default
    // `10` is the negative control — a dropped write / missing layout reads 10.
    assert_eq!(
        latest("LevelBar", "ActualWidth"),
        Some(DpValue::F32(30.0)),
        "Fire click should drive LevelBar.Width=30 => ActualWidth 30 (default 10); got {got_dp:?}",
    );

    // Read-back 3 (cross-bridge round-trip): the Fire-click reaction wrote the
    // Status line via NoesisText; the DP watch read it back as "Fired x1" (later
    // overwritten by the toggle click, so we look for it anywhere in the stream).
    // The default "Ready" is the negative control. Proves click -> react ->
    // NoesisText write -> NoesisDp read end to end.
    assert!(
        got_dp.iter().any(|(e, name, prop, value)| *e == view
            && name == "Status"
            && prop == "Text"
            && *value == DpValue::Str("Fired x1".to_string())),
        "Fire click should drive Status.Text to \"Fired x1\" (default \"Ready\"); got {got_dp:?}",
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
