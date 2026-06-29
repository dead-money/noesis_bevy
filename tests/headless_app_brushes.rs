//! Bevy-app-level integration test for the **code-built brush** bridge
//! ([`NoesisBrushes`]), exercised end-to-end through the real `NoesisPlugin`
//! pipeline (headless, pipelined rendering on).
//!
//! # Observable
//!
//! A brush assignment changes no scalar dependency property on the element it
//! paints, so there's no derived `NoesisDp` watch to lean on (the trick the
//! visibility/layout/focus tests use). Instead the bridge itself reads the
//! assigned brush back: after applying, it polls each painted target's live
//! `SolidColorBrush` color via `FrameworkElement::solid_brush_color` (a
//! `DynamicCast` round-trip through the element's `Background`/`Fill`/... DP) and
//! emits a [`NoesisBrushChanged`] carrying that color. This is bluff-resistant:
//!
//!   * An **unpainted** Border's `Background` is null, so the read-back is
//!     `None` and *no* message is emitted — a missing apply / wrong-entity
//!     routing / inverted change-detection stays silent and fails the assert.
//!   * The emitted color is read *from the live brush on the element*, not from
//!     the Rust-side spec, so it proves the brush actually landed — and the
//!     exact value (`[1, 0, 0, 1]`) differs from any default.
//!   * A **gradient** Fill is a `LinearGradientBrush`, not a `SolidColorBrush`,
//!     so the solid read-back is `None`: the gradient exercises the build +
//!     assign path but produces no solid-color message (asserted below).
//!
//! Font-free XAML (only brush state is asserted, no glyph rendering), so the
//! scene builds with no font gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::{
    BrushTarget, GradientStop, NoesisBrushChanged, NoesisBrushes, NoesisCamera, NoesisPlugin,
    NoesisView, XamlRegistry,
};

const SET_AT_FRAME: usize = 10;
const EXIT_AT_FRAME: usize = 60;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <Border x:Name="Panel" Width="32" Height="16"/>
  <Border x:Name="Other" Width="32" Height="16"/>
  <Rectangle x:Name="Bar" Width="20" Height="10"/>
</Grid>"##;

type Observed = Vec<(Entity, String, BrushTarget, [f32; 4])>;

#[test]
fn brushes_bridge_paints_and_reads_back() {
    noesis_license_from_env();

    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
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
    app.add_plugins(NoesisPlugin::default());

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "brushes.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "brushes.xaml".to_string(),
                        size: UVec2::new(64, 32),
                        ..default()
                    },
                    // Starts empty (no-op); filled in after the scene exists so
                    // its one-shot apply isn't lost.
                    NoesisBrushes::new(),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let observed_sys = Arc::clone(&observed);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut q: Query<&mut NoesisBrushes>,
              mut changes: MessageReader<NoesisBrushChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for mut brushes in &mut q {
                    *brushes = NoesisBrushes::new()
                        .solid("Panel", BrushTarget::Background, [1.0, 0.0, 0.0, 1.0])
                        .linear_gradient(
                            "Bar",
                            BrushTarget::Fill,
                            [0.0, 0.0],
                            [1.0, 0.0],
                            vec![
                                GradientStop::new(0.0, [0.0, 0.0, 0.0, 1.0]),
                                GradientStop::new(1.0, [1.0, 1.0, 1.0, 1.0]),
                            ],
                        );
                }
            }

            for ev in changes.read() {
                observed_sys
                    .lock()
                    .unwrap()
                    .push((ev.view, ev.name.clone(), ev.target, ev.color));
            }

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed NoesisBrushChanged ---");
    for (e, name, target, color) in &got {
        eprintln!("  {e:?} {name}.{} = {color:?}", target.property());
    }

    // The painted Border's Background reads back as the exact solid color we set.
    let panel = got
        .iter()
        .rfind(|(e, n, t, _)| *e == view && n == "Panel" && *t == BrushTarget::Background)
        .map(|(_, _, _, c)| *c);
    assert_eq!(
        panel,
        Some([1.0, 0.0, 0.0, 1.0]),
        "brushes: painting Panel.Background red should read back [1,0,0,1] \
         (an unpainted Border's Background is null => no message)",
    );

    // Negative control: an un-targeted Border must never surface a message — a
    // "paint everything" regression would light up Other.
    assert!(
        !got.iter().any(|(_, n, _, _)| n == "Other"),
        "brushes: an un-targeted element must not emit a brush read-back",
    );

    // The gradient Fill is a LinearGradientBrush, not a SolidColorBrush, so the
    // solid read-back is None and Bar emits no message (documents the bridge's
    // solid-only read-back; the gradient still builds + assigns without panic).
    assert!(
        !got.iter().any(|(_, n, _, _)| n == "Bar"),
        "brushes: a gradient Fill has no solid color and must not emit a \
         solid-color read-back",
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
