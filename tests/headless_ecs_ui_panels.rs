//! ECS-UI integration proof, **Primitive 1 (panel = entity)**: two `UiPanel`
//! instances of the *same* component set bind independently, and despawning one
//! reaps it with no leak. Asserted against the [`ecs_ui`] example's own scene +
//! component types, so this pins the exact code a user runs.
//!
//! One `#[test]` per file: each headless Noesis app owns the thread-affine runtime
//! for its whole process, so the integration tests never share a binary.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    NoesisCamera, NoesisDiagnostics, NoesisPanelAppExt, NoesisPanelText, NoesisPanelTextChanged,
    NoesisPlugin, NoesisView, UiPanel, XamlRegistry,
};

#[allow(dead_code)]
#[path = "../examples/ecs_ui.rs"]
mod ecs_ui;

use ecs_ui::{Health, Score};

const HEAL_AT: usize = 16;
const DESPAWN_AT: usize = 30;
const EXIT_AT: usize = 60;

#[test]
fn panels_multi_instance_isolate_and_reap() {
    noesis_license_from_env();

    // Latest (name -> text) per panel entity, captured from the read-back.
    type Captured = HashMap<Entity, HashMap<String, String>>;
    let captured: Arc<Mutex<Captured>> = Arc::new(Mutex::new(HashMap::new()));
    let panels: Arc<Mutex<Option<(Entity, Entity)>>> = Arc::new(Mutex::new(None));
    let live_before: Arc<Mutex<usize>> = Arc::new(Mutex::new(usize::MAX));
    let live_after: Arc<Mutex<usize>> = Arc::new(Mutex::new(usize::MAX));

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
    app.add_noesis_panel_field::<Health>()
        .add_noesis_panel_field::<Score>();

    let panels_startup = Arc::clone(&panels);
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
            // Two instances of the SAME component set, into the two host slots; the
            // example HUD fragment names its value TextBlocks, so we can read each
            // panel's bound value back out per entity.
            let watch = || {
                NoesisPanelText::new().watching([ecs_ui::HUD_HEALTH_VALUE, ecs_ui::HUD_SCORE_VALUE])
            };
            let p1 = commands
                .spawn((
                    UiPanel::new(ecs_ui::HUD_URI).mount_into(view, ecs_ui::HUD1_SLOT),
                    watch(),
                    Health(100.0),
                    Score(7),
                ))
                .id();
            let p2 = commands
                .spawn((
                    UiPanel::new(ecs_ui::HUD_URI).mount_into(view, ecs_ui::HUD2_SLOT),
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
              mut reads: MessageReader<NoesisPanelTextChanged>,
              mut exit: MessageWriter<AppExit>| {
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
            if *frame >= EXIT_AT {
                *after_sys.lock().unwrap() = diag.live_panels;
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let (p1, p2) = panels.lock().unwrap().expect("panels spawned");
    let snap = captured.lock().unwrap().clone();
    let a = snap.get(&p1).cloned().unwrap_or_default();
    let b = snap.get(&p2).cloned().unwrap_or_default();

    // p1 drove BOTH bindings from one aggregated DataContext.
    assert_eq!(
        a.get(ecs_ui::HUD_HEALTH_VALUE).map(String::as_str),
        Some("25"),
        "panel 1 Health (post-mutate) never reached the UI; p1 reads {a:?}",
    );
    assert_eq!(
        a.get(ecs_ui::HUD_SCORE_VALUE).map(String::as_str),
        Some("7"),
        "panel 1 Score never reached the UI; p1 reads {a:?}",
    );
    // Isolation: mutating p1 left p2 untouched.
    assert_eq!(
        b.get(ecs_ui::HUD_HEALTH_VALUE).map(String::as_str),
        Some("50"),
        "panel 2 Health was not isolated from panel 1's mutation; p2 reads {b:?}",
    );
    assert_eq!(
        b.get(ecs_ui::HUD_SCORE_VALUE).map(String::as_str),
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

fn noesis_license_from_env() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
}
