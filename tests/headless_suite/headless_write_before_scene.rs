//! A bridge write made before the view's scene exists must still land.
//!
//! Per-view bridges apply writes when the component `is_changed()`. A write made
//! before the scene exists (XAML still loading, fonts not staged) no-ops against
//! the missing scene and `is_changed` does not refire, so the bridges also
//! re-apply on `NoesisRenderState::scene_rebuilt_this_frame`. This test seeds a
//! [`NoesisText`] write at startup, registers the XAML several frames late so the
//! scene cannot build until then, and reads the value back through a
//! [`NoesisDp`] watch once it does.
//!
//! The read-back uses `NoesisDp` rather than `NoesisText`'s own watch on purpose:
//! the text bridge eagerly snapshots its own writes (to suppress phantom echoes),
//! which would also hide the applied value. The DP bridge keeps an independent
//! snapshot, so it reports the element's real `Text` after the write lands.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    DpKind, DpValue, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisText, NoesisView, XamlRegistry,
};

use crate::common::{headless_app, run_until};

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
    // (frame, text) for every NoesisDpChanged observed on TARGET.
    let observed: Arc<Mutex<Vec<(usize, String)>>> = Arc::new(Mutex::new(Vec::new()));

    let mut app = headless_app();

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
              mut changed: MessageReader<NoesisDpChanged>| {
            *frame += 1;
            if *frame == REGISTER_AT_FRAME {
                reg.insert("late.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            }
            for ev in changed.read() {
                if ev.name == TARGET
                    && ev.property == "Text"
                    && let DpValue::Str(text) = &ev.value
                {
                    observed_sys.lock().unwrap().push((*frame, text.clone()));
                }
            }
        },
    );

    let pred_observed = Arc::clone(&observed);
    let applied = run_until(&mut app, 240, move |_app| {
        pred_observed
            .lock()
            .unwrap()
            .iter()
            .any(|(_, text)| text == WRITTEN)
    });

    let seen = observed.lock().unwrap().clone();
    // The write must reach the element only after the late scene build, and the
    // final observed value must be the written one (not the XAML default).
    assert!(
        applied,
        "write never applied within 240 frames; observed read-backs: {seen:?}"
    );
    let applied_hit = seen.iter().find(|(_, text)| text == WRITTEN).unwrap();
    assert!(
        applied_hit.0 >= REGISTER_AT_FRAME,
        "write applied at frame {}, before the scene could build at {REGISTER_AT_FRAME}",
        applied_hit.0
    );
    let last = seen.last().map(|(_, t)| t.as_str());
    assert_eq!(
        last,
        Some(WRITTEN),
        "final read-back was not the written value"
    );
}
