//! ECS-UI integration proof, **F8 (deferred panel seal)**: a
//! `UiPanel::deferred_seal()` panel holds its `DataContext` freeze until a
//! `SealPanel` marker, so a bound component contributed a frame later (the
//! cross-module race) still joins the binding instead of being dropped. Asserted
//! against the [`ecs_ui`] example's HUD fragment (`{Binding Health}` /
//! `{Binding Score}`), so a late field that fails to bind reads back as absent.
//!
//! One `#[test]` per file: each headless Noesis app owns the thread-affine runtime
//! for its whole process, so the integration tests never share a binary.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    NoesisCamera, NoesisPanelAppExt, NoesisPanelText, NoesisPanelTextChanged, NoesisView,
    SealPanel, UiPanel, XamlRegistry,
};

mod common;
use common::{headless_app, run_until};

#[allow(dead_code)]
#[path = "../examples/ecs_ui.rs"]
mod ecs_ui;

use ecs_ui::{Health, Score};

// Score is contributed after first-sight would have frozen a default panel, then
// the panel is sealed once the contributor is done.
const ADD_SCORE_AT: usize = 8;
const SEAL_AT: usize = 12;

#[test]
fn deferred_seal_binds_a_late_added_field() {
    // Latest (name -> text) captured from the read-back.
    let captured: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
    let panel: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();
    app.add_noesis_panel_field::<Health>()
        .add_noesis_panel_field::<Score>();

    let panel_startup = Arc::clone(&panel);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            ecs_ui::register_xaml(&mut reg);
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: ecs_ui::HOST_URI.to_string(),
                        size: UVec2::new(640, 480),
                        ..default()
                    },
                ))
                .id();
            // Spawn with ONLY Health; Score is contributed late (the cross-module
            // case). `deferred_seal` keeps the panel from freezing on first sight,
            // so the late Score still joins the DataContext once we seal.
            let p = commands
                .spawn((
                    UiPanel::new(ecs_ui::HUD_URI)
                        .mount_into(view, ecs_ui::HUD1_SLOT)
                        .deferred_seal(),
                    NoesisPanelText::new()
                        .watching([ecs_ui::HUD_HEALTH_VALUE, ecs_ui::HUD_SCORE_VALUE]),
                    Health(100.0),
                ))
                .id();
            *panel_startup.lock().unwrap() = Some(p);
        },
    );

    let captured_sys = Arc::clone(&captured);
    let panel_sys = Arc::clone(&panel);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut commands: Commands,
              mut reads: MessageReader<NoesisPanelTextChanged>| {
            *frame += 1;
            for ev in reads.read() {
                captured_sys
                    .lock()
                    .unwrap()
                    .insert(ev.name.clone(), ev.text.clone());
            }
            let p = panel_sys.lock().unwrap().expect("panel spawned");
            if *frame == ADD_SCORE_AT {
                commands.entity(p).insert(Score(7));
            }
            if *frame == SEAL_AT {
                commands.entity(p).insert(SealPanel);
            }
        },
    );

    // Exit once BOTH the spawn-time Health and the late-added Score have bound and
    // read back through the panel. A broken deferred seal would freeze on Health
    // alone and Score would never arrive, timing this out.
    let pred_captured = Arc::clone(&captured);
    let bound = run_until(&mut app, 240, move |_app| {
        let snap = pred_captured.lock().unwrap();
        snap.get(ecs_ui::HUD_HEALTH_VALUE).map(String::as_str) == Some("100")
            && snap.get(ecs_ui::HUD_SCORE_VALUE).map(String::as_str) == Some("7")
    });

    let snap = captured.lock().unwrap().clone();
    assert!(
        bound,
        "deferred panel never bound both Health and the late-added Score within \
         240 frames; reads {snap:?}",
    );
    // The component present at spawn binds.
    assert_eq!(
        snap.get(ecs_ui::HUD_HEALTH_VALUE).map(String::as_str),
        Some("100"),
        "deferred panel's Health never bound; reads {snap:?}",
    );
    // And the LATE-added Score binds too. Without `deferred_seal`, the panel would
    // freeze with Health only and this would be empty/absent.
    assert_eq!(
        snap.get(ecs_ui::HUD_SCORE_VALUE).map(String::as_str),
        Some("7"),
        "late-added Score did not bind; the deferred seal didn't capture it; reads {snap:?}",
    );
}
