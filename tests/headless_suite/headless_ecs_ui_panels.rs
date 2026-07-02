//! ECS-UI integration proof, **Primitive 1 (panel = entity)**: two `UiPanel`
//! instances of the *same* component set bind independently, and despawning one
//! reaps it with no leak. Asserted against the [`ecs_ui`] example's own scene +
//! component types, so this pins the exact code a user runs.
//!
//! One `#[test]` per file: each headless Noesis app owns the thread-affine runtime
//! for its whole process, so the integration tests never share a binary.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    NoesisCamera, NoesisDiagnostics, NoesisPanelAppExt, NoesisPanelText, NoesisPanelTextChanged,
    NoesisView, UiPanel, XamlRegistry,
};

use crate::common::{headless_app, run_until};

use crate::ecs_ui::{Health, Score};

// Stimulus timings: heal p1, then despawn p2. The run's exit is the terminal
// predicate (both panels' values read back, isolated, and p2 reaped).
const HEAL_AT: usize = 16;
const DESPAWN_AT: usize = 30;

#[test]
fn panels_multi_instance_isolate_and_reap() {
    // Latest (name -> text) per panel entity, captured from the read-back.
    type Captured = HashMap<Entity, HashMap<String, String>>;
    let captured: Arc<Mutex<Captured>> = Arc::new(Mutex::new(HashMap::new()));
    let panels: Arc<Mutex<Option<(Entity, Entity)>>> = Arc::new(Mutex::new(None));
    let live_before: Arc<Mutex<usize>> = Arc::new(Mutex::new(usize::MAX));
    // Live-panel count after the p2 despawn, refreshed every frame for the exit
    // predicate; drops to 1 once the reap lands.
    let live_after: Arc<Mutex<usize>> = Arc::new(Mutex::new(usize::MAX));

    let mut app = headless_app();
    app.add_noesis_panel_field::<Health>()
        .add_noesis_panel_field::<Score>();

    let panels_startup = Arc::clone(&panels);
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
            // Two instances of the SAME component set, into the two host slots; the
            // example HUD fragment names its value TextBlocks, so we can read each
            // panel's bound value back out per entity.
            let watch = || {
                NoesisPanelText::new().watching([
                    crate::ecs_ui::HUD_HEALTH_VALUE,
                    crate::ecs_ui::HUD_SCORE_VALUE,
                ])
            };
            let p1 = commands
                .spawn((
                    UiPanel::new(crate::ecs_ui::HUD_URI).mount_into(view, crate::ecs_ui::HUD1_SLOT),
                    watch(),
                    Health(100.0),
                    Score(7),
                ))
                .id();
            let p2 = commands
                .spawn((
                    UiPanel::new(crate::ecs_ui::HUD_URI).mount_into(view, crate::ecs_ui::HUD2_SLOT),
                    watch(),
                    Health(50.0),
                    Score(3),
                ))
                .id();
            *panels_startup.lock().unwrap() = Some((p1, p2));
        },
    );

    let captured_sys = Arc::clone(&captured);
    let panels_sys = Arc::clone(&panels);
    let before_sys = Arc::clone(&live_before);
    let after_sys = Arc::clone(&live_after);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut commands: Commands,
              diag: Res<NoesisDiagnostics>,
              mut healths: Query<&mut Health>,
              mut reads: MessageReader<NoesisPanelTextChanged>| {
            *frame += 1;
            for ev in reads.read() {
                captured_sys
                    .lock()
                    .unwrap()
                    .entry(ev.panel)
                    .or_default()
                    .insert(ev.name.clone(), ev.text.clone());
            }
            let (p1, p2) = panels_sys.lock().unwrap().expect("panels spawned");
            // Mutate ONLY p1's Health with a plain query; p2 must stay isolated.
            if *frame == HEAL_AT
                && let Ok(mut hp) = healths.get_mut(p1)
            {
                hp.0 = 25.0;
            }
            let _ = p2;
            if *frame == DESPAWN_AT {
                *before_sys.lock().unwrap() = diag.live_panels;
                commands.entity(p2).despawn();
            }
            // After the despawn, track the live-panel count so the predicate sees
            // the reap drop it to 1.
            if *frame > DESPAWN_AT {
                *after_sys.lock().unwrap() = diag.live_panels;
            }
        },
    );

    // Exit once both panels' values have read back, p1's mutation is isolated from
    // p2, and the despawned p2 has been reaped (2 live before -> 1 after).
    let pred_captured = Arc::clone(&captured);
    let pred_panels = Arc::clone(&panels);
    let pred_before = Arc::clone(&live_before);
    let pred_after = Arc::clone(&live_after);
    let settled = run_until(&mut app, 240, move |_app| {
        let Some((p1, p2)) = *pred_panels.lock().unwrap() else {
            return false;
        };
        let snap = pred_captured.lock().unwrap();
        let val = |e: Entity, name: &str, want: &str| {
            snap.get(&e)
                .and_then(|m| m.get(name))
                .is_some_and(|t| t == want)
        };
        val(p1, crate::ecs_ui::HUD_HEALTH_VALUE, "25")
            && val(p1, crate::ecs_ui::HUD_SCORE_VALUE, "7")
            && val(p2, crate::ecs_ui::HUD_HEALTH_VALUE, "50")
            && val(p2, crate::ecs_ui::HUD_SCORE_VALUE, "3")
            && *pred_before.lock().unwrap() == 2
            && *pred_after.lock().unwrap() == 1
    });

    let (p1, p2) = panels.lock().unwrap().expect("panels spawned");
    let snap = captured.lock().unwrap().clone();
    let a = snap.get(&p1).cloned().unwrap_or_default();
    let b = snap.get(&p2).cloned().unwrap_or_default();

    assert!(
        settled,
        "panels never reached the terminal state (both values read back, isolated, \
         p2 reaped) within 240 frames; p1={a:?} p2={b:?}",
    );

    // p1 drove BOTH bindings from one aggregated DataContext.
    assert_eq!(
        a.get(crate::ecs_ui::HUD_HEALTH_VALUE).map(String::as_str),
        Some("25"),
        "panel 1 Health (post-mutate) never reached the UI; p1 reads {a:?}",
    );
    assert_eq!(
        a.get(crate::ecs_ui::HUD_SCORE_VALUE).map(String::as_str),
        Some("7"),
        "panel 1 Score never reached the UI; p1 reads {a:?}",
    );
    // Isolation: mutating p1 left p2 untouched.
    assert_eq!(
        b.get(crate::ecs_ui::HUD_HEALTH_VALUE).map(String::as_str),
        Some("50"),
        "panel 2 Health was not isolated from panel 1's mutation; p2 reads {b:?}",
    );
    assert_eq!(
        b.get(crate::ecs_ui::HUD_SCORE_VALUE).map(String::as_str),
        Some("3"),
        "panel 2 Score never reached the UI; p2 reads {b:?}",
    );
    // Reap: two panels live before despawn, one after, no leak.
    assert_eq!(
        *live_before.lock().unwrap(),
        2,
        "expected 2 live panels pre-despawn",
    );
    assert_eq!(
        *live_after.lock().unwrap(),
        1,
        "despawned panel was not reaped (live_panels did not drop to 1)",
    );
}
