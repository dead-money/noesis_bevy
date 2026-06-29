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
//! brush DP and emits a [`NoesisBrushChanged`] carrying a [`BrushReadback`]:
//!
//!   * [`BrushReadback::Solid`] — read off the live `SolidColorBrush` via a
//!     `DynamicCast` round-trip through the element's `Background`/`Fill`/...
//!     DP. The color comes *from the element*, not the Rust-side spec, so it
//!     proves the brush actually landed; the exact value differs from any
//!     default.
//!   * [`BrushReadback::NonSolid`] — a live brush is present but is not a
//!     `SolidColorBrush` (our gradient). Proves the gradient build + assign
//!     landed even though the runtime exposes no safe per-DP gradient-stop
//!     read-back to the `unsafe_code = forbid` crate.
//!   * An **unpainted** / failed-assign target has a null DP, so the read-back
//!     is *nothing* and no message is emitted — a missing apply / wrong-entity
//!     routing / inverted change-detection stays silent and fails the assert.
//!
//! This test paints all four [`BrushTarget`]s end-to-end (Background, Foreground,
//! Fill, Stroke), uses distinct-per-channel colors so a swapped/zeroed channel
//! is caught, and paints two elements (and two targets on one element) with
//! *different* colors to catch per-key cross-contamination.
//!
//! Font-free XAML (only brush state is asserted, no glyph rendering), so the
//! scene builds with no font gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::{
    BrushReadback, BrushTarget, GradientStop, NoesisBrushChanged, NoesisBrushes, NoesisCamera,
    NoesisPlugin, NoesisView, XamlRegistry,
};

const SET_AT_FRAME: usize = 10;
const EXIT_AT_FRAME: usize = 60;

// Distinct-per-channel colors: every channel differs within a color *and* across
// colors, so a swapped channel, a zeroed channel, or a cross-element/cross-target
// contamination all read back wrong and fail the exact assert.
const PANEL_BG: [f32; 4] = [0.2, 0.4, 0.6, 0.8];
const PANEL2_BG: [f32; 4] = [0.6, 0.2, 0.8, 0.4];
const LABEL_FG: [f32; 4] = [0.1, 0.3, 0.5, 0.7];
const BAR_FILL: [f32; 4] = [0.9, 0.7, 0.5, 0.3];
const BAR_STROKE: [f32; 4] = [0.3, 0.9, 0.1, 0.5];

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="128" Height="64">
  <Border x:Name="Panel" Width="32" Height="16"/>
  <Border x:Name="Panel2" Width="32" Height="16"/>
  <Border x:Name="Other" Width="32" Height="16"/>
  <TextBlock x:Name="Label" Text="x"/>
  <Rectangle x:Name="Bar" Width="20" Height="10" StrokeThickness="2"/>
  <Rectangle x:Name="Grad" Width="20" Height="10"/>
</Grid>"##;

type Observed = Vec<(Entity, String, BrushTarget, BrushReadback)>;

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
                        size: UVec2::new(128, 64),
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
                        // All four targets, end-to-end, distinct colors.
                        .solid("Panel", BrushTarget::Background, PANEL_BG)
                        .solid("Panel2", BrushTarget::Background, PANEL2_BG)
                        .solid("Label", BrushTarget::Foreground, LABEL_FG)
                        .solid("Bar", BrushTarget::Fill, BAR_FILL)
                        .solid("Bar", BrushTarget::Stroke, BAR_STROKE)
                        // A gradient Fill on a *different* element: read back as
                        // NonSolid, and must not contaminate Bar's solid Fill.
                        .linear_gradient(
                            "Grad",
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
                observed_sys.lock().unwrap().push((
                    ev.view,
                    ev.name.clone(),
                    ev.target,
                    ev.readback,
                ));
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
    for (e, name, target, readback) in &got {
        eprintln!("  {e:?} {name}.{} = {readback:?}", target.property());
    }

    // Latest readback for a given (name, target) on this view, if any was emitted.
    let last = |name: &str, target: BrushTarget| -> Option<BrushReadback> {
        got.iter()
            .rfind(|(e, n, t, _)| *e == view && n == name && *t == target)
            .map(|(_, _, _, r)| *r)
    };

    // Every solid target reads back its OWN exact color. Distinct per-channel
    // values mean a swapped/zeroed channel or cross-key contamination fails here.
    assert_eq!(
        last("Panel", BrushTarget::Background),
        Some(BrushReadback::Solid(PANEL_BG)),
        "Panel.Background must read back its own solid color",
    );
    assert_eq!(
        last("Panel2", BrushTarget::Background),
        Some(BrushReadback::Solid(PANEL2_BG)),
        "Panel2.Background must read back its own solid color (catches \
         per-key cross-contamination with Panel)",
    );
    assert_eq!(
        last("Label", BrushTarget::Foreground),
        Some(BrushReadback::Solid(LABEL_FG)),
        "Label.Foreground must read back its own solid color (Foreground \
         target proven end-to-end)",
    );
    assert_eq!(
        last("Bar", BrushTarget::Fill),
        Some(BrushReadback::Solid(BAR_FILL)),
        "Bar.Fill must read back its own solid color (Fill target proven \
         end-to-end)",
    );
    assert_eq!(
        last("Bar", BrushTarget::Stroke),
        Some(BrushReadback::Solid(BAR_STROKE)),
        "Bar.Stroke must read back its own solid color, distinct from Bar.Fill \
         (Stroke target proven; catches Fill/Stroke contamination on one element)",
    );

    // The gradient Fill landed: a live brush is present but it is not a
    // SolidColorBrush, so it reports NonSolid (a no-op gradient would leave the
    // DP null and emit nothing — this fails if the build/assign silently dropped).
    assert_eq!(
        last("Grad", BrushTarget::Fill),
        Some(BrushReadback::NonSolid),
        "Grad.Fill gradient must land and read back as a non-solid brush \
         (asserts the strongest signal available; the runtime exposes no safe \
         per-DP gradient-stop read-back to this unsafe-free crate)",
    );

    // Negative control: an un-targeted Border must never surface a message — a
    // "paint everything" regression would light up Other.
    assert!(
        !got.iter().any(|(_, n, _, _)| n == "Other"),
        "an un-targeted element must not emit a brush read-back",
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
