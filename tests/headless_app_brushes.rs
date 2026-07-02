//! Integration test for the brush bridge ([`NoesisBrushes`]) through the real
//! `NoesisPlugin` pipeline (headless, pipelined rendering on).
//!
//! Brush assignment changes no scalar DP on the painted element, so there is no
//! `NoesisDp` watch to use (the approach the visibility and focus tests take).
//! The bridge reads the assigned brush back from the element's live DP and emits
//! [`NoesisBrushChanged`]. A null DP (failed assign, wrong-entity routing, or
//! inverted change-detection) emits nothing, so those failures stay silent and
//! fail the assert.
//!
//! Colors are distinct per-channel to catch swapped or zeroed channels and
//! cross-key contamination across elements. Gradient landing is confirmed as
//! [`BrushReadback::NonSolid`]: the runtime exposes no per-DP gradient-stop
//! read-back to `unsafe_code = forbid` code.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    BrushReadback, BrushTarget, GradientStop, NoesisBrushChanged, NoesisBrushes, NoesisCamera,
    NoesisView, XamlRegistry,
};

mod common;
use common::{headless_app, run_until};

const SET_AT_FRAME: usize = 10;

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
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

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
              mut changes: MessageReader<NoesisBrushChanged>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for mut brushes in &mut q {
                    *brushes = NoesisBrushes::new()
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
        },
    );

    // The latest readback for a (view, name, target) triple.
    let last_for =
        |got: &Observed, view: Entity, name: &str, target: BrushTarget| -> Option<BrushReadback> {
            got.iter()
                .rfind(|(e, n, t, _)| *e == view && n == name && *t == target)
                .map(|(_, _, _, r)| *r)
        };

    // Event-driven exit: stop once every painted target has read back (the five
    // solids plus the gradient), not after a padded frame count.
    let pred_observed = Arc::clone(&observed);
    let pred_view = Arc::clone(&view_entity);
    let painted = run_until(&mut app, 240, move |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        let got = pred_observed.lock().unwrap();
        last_for(&got, view, "Panel", BrushTarget::Background)
            == Some(BrushReadback::Solid(PANEL_BG))
            && last_for(&got, view, "Panel2", BrushTarget::Background)
                == Some(BrushReadback::Solid(PANEL2_BG))
            && last_for(&got, view, "Label", BrushTarget::Foreground)
                == Some(BrushReadback::Solid(LABEL_FG))
            && last_for(&got, view, "Bar", BrushTarget::Fill)
                == Some(BrushReadback::Solid(BAR_FILL))
            && last_for(&got, view, "Bar", BrushTarget::Stroke)
                == Some(BrushReadback::Solid(BAR_STROKE))
            && last_for(&got, view, "Grad", BrushTarget::Fill) == Some(BrushReadback::NonSolid)
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed NoesisBrushChanged ---");
    for (e, name, target, readback) in &got {
        eprintln!("  {e:?} {name}.{} = {readback:?}", target.property());
    }

    let last = |name: &str, target: BrushTarget| -> Option<BrushReadback> {
        last_for(&got, view, name, target)
    };

    assert!(
        painted,
        "brush read-backs never all landed within 240 frames; observed {got:?}",
    );

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

    assert_eq!(
        last("Grad", BrushTarget::Fill),
        Some(BrushReadback::NonSolid),
        "Grad.Fill gradient must land and read back as a non-solid brush \
         (asserts the strongest signal available; the runtime exposes no safe \
         per-DP gradient-stop read-back to this unsafe-free crate)",
    );

    // Negative control: an un-targeted Border must not emit a message; a "paint everything" regression would light up Other.
    assert!(
        !got.iter().any(|(_, n, _, _)| n == "Other"),
        "an un-targeted element must not emit a brush read-back",
    );
}
