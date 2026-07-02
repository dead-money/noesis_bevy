//! End-to-end test of Primitive 1 (**panel = entity**) through the Bevy app.
//!
//! A host [`NoesisView`] scene carries a named `StackPanel` (`x:Name="Hud"`). Each
//! [`UiPanel`] entity loads `hud.xaml` (a fragment binding `{Binding Health}` and
//! `{Binding Score}`), aggregates its two bound components (`Health(f32)`,
//! `Score(i32)`) into one `DataContext`, and mounts into `Hud`.
//!
//! Three properties under test:
//!   * **Aggregation.** One panel with *two* bound components drives *both*
//!     bindings from one `DataContext` (neither overwrites the other).
//!   * **Isolation.** Two panels of the *same* component set bind independently:
//!     mutating panel A's `Health` leaves panel B's untouched.
//!   * **Reap.** Despawning a panel removes its mounted child from the host
//!     (`live_panels` drops back), with no leak / crash.
//!
//! Each panel's bound values are read back out of its fragment via
//! [`NoesisPanelText`] (a fragment-scope `Text` watch), proving the bindings
//! reached the UI. A mounted fragment keeps a private namescope, so the watch
//! names are fragment-local (`"HealthText"`, `"ScoreText"`) and the read-back is
//! keyed by the originating panel entity.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    NoesisCamera, NoesisDiagnostics, NoesisPanelAppExt, NoesisPanelText, NoesisPanelTextChanged,
    NoesisView, NoesisViewModel, UiPanel, XamlRegistry,
};

use crate::common::{headless_app, run_until};

// Host scene: one named StackPanel that panels mount into.
const HOST_XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="256" Height="256">
  <StackPanel x:Name="Hud"/>
</Grid>"##;

// Sub-XAML fragment, loaded once per panel; its DataContext is the panel entity's
// aggregated components. Two bindings prove aggregation; each mounted copy gets
// its own namescope, so "HealthText"/"ScoreText" resolve per-fragment.
const HUD_XAML: &str = r##"<StackPanel xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml">
  <TextBlock x:Name="HealthText" Text="{Binding Health}"/>
  <TextBlock x:Name="ScoreText" Text="{Binding Score}"/>
</StackPanel>"##;

/// Type-named newtype: binds `{Binding Health}`.
#[derive(Component, NoesisViewModel)]
struct Health(f32);

/// Type-named newtype: binds `{Binding Score}`.
#[derive(Component, NoesisViewModel)]
struct Score(i32);

// Frame-gated stimulus: heal panel A, then despawn panel B. Frames are instant
// under run_until; the exit predicate is the terminal aggregate/isolate/reap
// state, not a fixed frame count.
const HEAL_AT: usize = 16;
const DESPAWN_AT: usize = 30;

#[test]
fn panel_entity_aggregates_isolates_and_reaps() {
    // Latest (name -> text) per panel entity, captured from the read-back.
    type Captured = HashMap<Entity, HashMap<String, String>>;
    let captured: Arc<Mutex<Captured>> = Arc::new(Mutex::new(HashMap::new()));
    let entities: Arc<Mutex<Option<(Entity, Entity)>>> = Arc::new(Mutex::new(None));
    let baseline_live: Arc<Mutex<usize>> = Arc::new(Mutex::new(usize::MAX));
    // Live panel count, refreshed every frame once the despawn has fired.
    let final_live: Arc<Mutex<usize>> = Arc::new(Mutex::new(usize::MAX));

    let mut app = headless_app();
    app.add_noesis_panel_field::<Health>()
        .add_noesis_panel_field::<Score>();

    let entities_startup = Arc::clone(&entities);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "host.xaml".to_string(),
                Arc::new(HOST_XAML.as_bytes().to_vec()),
            );
            reg.insert(
                "hud.xaml".to_string(),
                Arc::new(HUD_XAML.as_bytes().to_vec()),
            );

            // Host view: the shared parent View with the named Hud panel.
            let host = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "host.xaml".to_string(),
                        size: UVec2::new(256, 256),
                        ..default()
                    },
                ))
                .id();

            // Panel A: two bound components, one aggregated DataContext.
            let a = commands
                .spawn((
                    UiPanel::new("hud.xaml").mount_into(host, "Hud"),
                    NoesisPanelText::new().watching(["HealthText", "ScoreText"]),
                    Health(100.0),
                    Score(7),
                ))
                .id();
            // Panel B: same component set, independent instance.
            let b = commands
                .spawn((
                    UiPanel::new("hud.xaml").mount_into(host, "Hud"),
                    NoesisPanelText::new().watching(["HealthText", "ScoreText"]),
                    Health(50.0),
                    Score(3),
                ))
                .id();
            *entities_startup.lock().unwrap() = Some((a, b));
        },
    );

    let captured_sys = Arc::clone(&captured);
    let entities_sys = Arc::clone(&entities);
    let baseline_sys = Arc::clone(&baseline_live);
    let final_sys = Arc::clone(&final_live);
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

            let (panel_a, panel_b) = entities_sys.lock().unwrap().expect("panels spawned");

            // Mutate ONLY panel A's Health; panel B must stay isolated.
            if *frame == HEAL_AT {
                if let Ok(mut hp) = healths.get_mut(panel_a) {
                    hp.0 = 25.0;
                }
            }

            // Record live panel count just before despawn (baseline = 2), then despawn.
            if *frame == DESPAWN_AT {
                *baseline_sys.lock().unwrap() = diag.live_panels;
                commands.entity(panel_b).despawn();
            }

            // After the despawn, keep the post-reap count current for the predicate.
            if *frame > DESPAWN_AT {
                *final_sys.lock().unwrap() = diag.live_panels;
            }
        },
    );

    // Exit once aggregation + isolation reads have landed and the reap has settled
    // (baseline 2 captured, live count back to 1).
    let pred_captured = Arc::clone(&captured);
    let pred_entities = Arc::clone(&entities);
    let pred_baseline = Arc::clone(&baseline_live);
    let pred_final = Arc::clone(&final_live);
    let completed = run_until(&mut app, 240, |_app| {
        let Some((panel_a, panel_b)) = *pred_entities.lock().unwrap() else {
            return false;
        };
        let snap = pred_captured.lock().unwrap();
        let a = snap.get(&panel_a);
        let b = snap.get(&panel_b);
        let a_ready = a.is_some_and(|m| {
            m.get("HealthText").map(String::as_str) == Some("25")
                && m.get("ScoreText").map(String::as_str) == Some("7")
        });
        let b_ready = b.is_some_and(|m| {
            m.get("HealthText").map(String::as_str) == Some("50")
                && m.get("ScoreText").map(String::as_str) == Some("3")
        });
        a_ready
            && b_ready
            && *pred_baseline.lock().unwrap() == 2
            && *pred_final.lock().unwrap() == 1
    });

    let (panel_a, panel_b) = entities.lock().unwrap().expect("panels spawned");
    let snap = captured.lock().unwrap().clone();
    let a = snap.get(&panel_a).cloned().unwrap_or_default();
    let b = snap.get(&panel_b).cloned().unwrap_or_default();
    let baseline = *baseline_live.lock().unwrap();
    let final_count = *final_live.lock().unwrap();

    assert!(
        completed,
        "panel scenario never reached its terminal state (aggregate + isolate + \
         reap) within 240 frames; A {a:?} B {b:?} baseline {baseline} final {final_count}",
    );

    // Aggregation: panel A drove BOTH bindings from one DataContext.
    assert_eq!(
        a.get("HealthText").map(String::as_str),
        Some("25"),
        "panel A Health binding (post-heal) never reached the UI; panel A reads {a:?}",
    );
    assert_eq!(
        a.get("ScoreText").map(String::as_str),
        Some("7"),
        "panel A Score binding never reached the UI; panel A reads {a:?}",
    );

    // Isolation: mutating A's Health left B's Health (and Score) untouched.
    assert_eq!(
        b.get("HealthText").map(String::as_str),
        Some("50"),
        "panel B Health was not isolated from panel A's mutation; panel B reads {b:?}",
    );
    assert_eq!(
        b.get("ScoreText").map(String::as_str),
        Some("3"),
        "panel B Score binding never reached the UI; panel B reads {b:?}",
    );

    // Reap: two panels live before despawn, one after.
    assert_eq!(baseline, 2, "expected 2 live panels before despawn");
    assert_eq!(
        final_count, 1,
        "despawned panel was not reaped (live_panels did not drop to 1)",
    );
}
