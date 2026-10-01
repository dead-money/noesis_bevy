//! Deferred panel seal: a `UiPanel::deferred_seal()` panel holds its
//! `DataContext` freeze until a `SealPanel` marker, so a bound component
//! contributed later (for example by another plugin) still joins the binding
//! instead of being dropped. Asserted against the `ecs_ui` example's HUD fragment
//! (`{Binding Health}` / `{Binding Score}`), so a late field that fails to bind
//! reads back as absent.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    NoesisCamera, NoesisPanelAppExt, NoesisPanelText, NoesisPanelTextChanged, NoesisView,
    SealPanel, UiPanel, XamlRegistry,
};

use crate::common::{headless_app, run_until};

use crate::ecs_ui::{Health, Score};

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
            crate::ecs_ui::register_xaml(&mut reg);
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: crate::ecs_ui::HOST_URI.to_string(),
                        size: UVec2::new(640, 480),
                        ..default()
                    },
                ))
                .id();
            // Spawn with only Health; Score is contributed late. `deferred_seal`
            // keeps the panel from freezing on first sight.
            let p = commands
                .spawn((
                    UiPanel::new(crate::ecs_ui::HUD_URI)
                        .mount_into(view, crate::ecs_ui::HUD1_SLOT)
                        .deferred_seal(),
                    NoesisPanelText::new().watching([
                        crate::ecs_ui::HUD_HEALTH_VALUE,
                        crate::ecs_ui::HUD_SCORE_VALUE,
                    ]),
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

    // A broken deferred seal freezes on Health alone and Score never arrives.
    let pred_captured = Arc::clone(&captured);
    let bound = run_until(&mut app, 240, move |_app| {
        let snap = pred_captured.lock().unwrap();
        snap.get(crate::ecs_ui::HUD_HEALTH_VALUE)
            .map(String::as_str)
            == Some("100")
            && snap.get(crate::ecs_ui::HUD_SCORE_VALUE).map(String::as_str) == Some("7")
    });

    let snap = captured.lock().unwrap().clone();
    assert!(
        bound,
        "deferred panel never bound both Health and the late-added Score within \
         240 frames; reads {snap:?}",
    );
    assert_eq!(
        snap.get(crate::ecs_ui::HUD_HEALTH_VALUE)
            .map(String::as_str),
        Some("100"),
        "deferred panel's Health never bound; reads {snap:?}",
    );
    // Without `deferred_seal`, the panel would freeze with Health only.
    assert_eq!(
        snap.get(crate::ecs_ui::HUD_SCORE_VALUE).map(String::as_str),
        Some("7"),
        "late-added Score did not bind; the deferred seal didn't capture it; reads {snap:?}",
    );
}
