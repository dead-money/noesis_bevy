//! Regression test for the "write set before the scene builds" footgun.
//!
//! Per-view bridges apply their writes through Bevy change detection: the
//! reconcile system calls `apply_*_for` when the component `is_changed()`. If the
//! component is set before the view's scene exists (XAML still loading, fonts not
//! staged), that apply no-ops against the missing scene and `is_changed` never
//! refires, so the write is silently dropped.
//!
//! The fix gives [`NoesisRenderState::scene_rebuilt_this_frame`] to the bridges:
//! a freshly built scene re-applies the component's current state even when it
//! did not change that frame. This test seeds a [`NoesisText`] write at startup,
//! registers the XAML several frames LATE so the scene cannot build until then,
//! and reads the value back through a [`NoesisDp`] watch once it does.
//!
//! The read-back uses `NoesisDp` rather than `NoesisText`'s own watch on purpose:
//! the text bridge eagerly snapshots its own writes (to suppress phantom echoes),
//! which would also hide the applied value. The DP bridge keeps an independent
//! snapshot, so it reports the element's real `Text` after the write lands.
//!
//!   `cargo test -p noesis_bevy --test headless_write_before_scene -- --nocapture`

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    DpKind, DpValue, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisPlugin, NoesisText, NoesisView,
    XamlRegistry,
};

const FRAMES: usize = 60;
const REGISTER_AT_FRAME: usize = 8;
const TARGET: &str = "Label";
const WRITTEN: &str = "Applied";

// Default text differs from the written value, so an unapplied write reads back
// the XAML default and the assertion fails.
const XAML: &str = r##"<Border xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
    xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml">
    <TextBlock x:Name="Label" Text="default"/>
</Border>"##;

#[test]
fn write_set_before_scene_builds_still_lands() {
    noesis_runtime::set_license(
        &std::env::var("NOESIS_LICENSE_NAME").unwrap_or_default(),
        &std::env::var("NOESIS_LICENSE_KEY").unwrap_or_default(),
    );

    // (frame, text) for every NoesisTextChanged observed on TARGET.
    let observed: Arc<Mutex<Vec<(usize, String)>>> = Arc::new(Mutex::new(Vec::new()));

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

    // Spawn the view and seed the write at startup, but do NOT register the XAML
    // yet: the scene cannot build, so the write's `is_changed` is consumed against
    // a missing scene.
    app.add_systems(Startup, |mut commands: Commands| {
        commands.spawn((
            Camera2d,
            NoesisCamera,
            NoesisView {
                xaml_uri: "late.xaml".to_string(),
                size: UVec2::new(128, 128),
                ..default()
            },
            NoesisText::new().with(TARGET, WRITTEN),
            NoesisDp::new().watch(TARGET, "Text", DpKind::Str),
        ));
    });

    let observed_sys = Arc::clone(&observed);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut reg: ResMut<XamlRegistry>,
              mut changed: MessageReader<NoesisDpChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;
            if *frame == REGISTER_AT_FRAME {
                reg.insert("late.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            }
            for ev in changed.read() {
                if ev.name == TARGET && ev.property == "Text" {
                    if let DpValue::Str(text) = &ev.value {
                        observed_sys.lock().unwrap().push((*frame, text.clone()));
                    }
                }
            }
            if *frame >= FRAMES {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let seen = observed.lock().unwrap().clone();
    // The write must reach the element only after the late scene build, and the
    // final observed value must be the written one (not the XAML default).
    let applied = seen.iter().find(|(_, text)| text == WRITTEN);
    assert!(
        applied.is_some(),
        "write never applied; observed read-backs: {seen:?}"
    );
    let (applied_frame, _) = applied.unwrap();
    assert!(
        *applied_frame >= REGISTER_AT_FRAME,
        "write applied at frame {applied_frame}, before the scene could build at {REGISTER_AT_FRAME}"
    );
    let last = seen.last().map(|(_, t)| t.as_str());
    assert_eq!(
        last,
        Some(WRITTEN),
        "final read-back was not the written value"
    );
}
