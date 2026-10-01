//! End-to-end test of panels as entities ([`UiPanel`]) on the headless harness.
//!
//! A host [`NoesisView`] scene carries a named `StackPanel` (`x:Name="Hud"`). Each
//! [`UiPanel`] entity loads `hud.xaml` (a fragment binding `{Binding Health}` and
//! `{Binding Score}`), aggregates its two bound components (`Health(f32)`,
//! `Score(i32)`) into one `DataContext`, and mounts into `Hud`.
//!
//! Three properties under test:
//!   * Aggregation: one panel with two bound components drives both bindings from
//!     one `DataContext`.
//!   * Isolation: two panels with the same component set bind independently;
//!     mutating panel A's `Health` leaves panel B's untouched.
//!   * Reap: despawning a panel unmounts it (`live_panels` drops from 2 to 1).
//!
//! Each panel's bound values are read back from its fragment via
//! [`NoesisPanelText`]. A mounted fragment keeps a private namescope, so the watch
//! names (`"HealthText"`, `"ScoreText"`) are fragment-local and each read-back
//! carries its panel entity.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    NoesisCamera, NoesisDiagnostics, NoesisPanelAppExt, NoesisPanelText, NoesisPanelTextChanged,
    NoesisView, NoesisViewModel, UiPanel, XamlRegistry,
};

use crate::common::{headless_app, run_until};

const HOST_XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="256" Height="256">
  <StackPanel x:Name="Hud"/>
</Grid>"##;

const HUD_XAML: &str = r##"<StackPanel xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml">
  <TextBlock x:Name="HealthText" Text="{Binding Health}"/>
  <TextBlock x:Name="ScoreText" Text="{Binding Score}"/>
</StackPanel>"##;

/// Newtype named after its binding: `{Binding Health}`.
#[derive(Component, NoesisViewModel)]
struct Health(f32);

/// Newtype named after its binding: `{Binding Score}`.
#[derive(Component, NoesisViewModel)]
struct Score(i32);

const HEAL_AT: usize = 16;
const DESPAWN_AT: usize = 30;

#[test]
fn panel_entity_aggregates_isolates_and_reaps() {
    // Latest text per element name, per panel entity.
    type Captured = HashMap<Entity, HashMap<String, String>>;
    let captured: Arc<Mutex<Captured>> = Arc::new(Mutex::new(HashMap::new()));
    let entities: Arc<Mutex<Option<(Entity, Entity)>>> = Arc::new(Mutex::new(None));
    let baseline_live: Arc<Mutex<usize>> = Arc::new(Mutex::new(usize::MAX));
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

            let a = commands
                .spawn((
                    UiPanel::new("hud.xaml").mount_into(host, "Hud"),
                    NoesisPanelText::new().watching(["HealthText", "ScoreText"]),
                    Health(100.0),
                    Score(7),
                ))
                .id();
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

            if *frame == HEAL_AT {
                if let Ok(mut hp) = healths.get_mut(panel_a) {
                    hp.0 = 25.0;
                }
            }

            if *frame == DESPAWN_AT {
                *baseline_sys.lock().unwrap() = diag.live_panels;
                commands.entity(panel_b).despawn();
            }

            if *frame > DESPAWN_AT {
                *final_sys.lock().unwrap() = diag.live_panels;
            }
        },
    );

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

    assert_eq!(baseline, 2, "expected 2 live panels before despawn");
    assert_eq!(
        final_count, 1,
        "despawned panel was not reaped (live_panels did not drop to 1)",
    );
}
