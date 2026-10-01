//! Integration tests for the [`NoesisTransform`] bridge, run headless through the real `NoesisPlugin`.
//!
//! `RenderTransform` is post-layout (no `ActualWidth`/`ActualHeight` change) and lives on a nested
//! `CompositeTransform` object, not reachable through `NoesisDp`. The bridge reads the element's
//! live `RenderTransform` back from Noesis after assigning it, gated on pointer identity.
//!
//! Positive: assigns translate(50,30)/scale(2,3)/rotate(45) to `Box` and asserts exact values
//! round-trip through [`NoesisTransformChanged`].
//! Negative: `Other` receives no transform and must never appear in any change event.
//!
//! `NoesisTransform` starts empty and is assigned at frame `SET_AT_FRAME`, after the scene exists.
//! Assigning before the view is live drops the one-shot change-detection apply.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    NoesisCamera, NoesisTransform, NoesisTransformChanged, NoesisView, TransformSpec, XamlRegistry,
};

use crate::common::{headless_app, run_until};

// Stimulus timing: assign after the scene is live (a pre-scene write loses the
// one-shot change-detection apply). The run's exit is the read-back predicate.
const SET_AT_FRAME: usize = 10;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <Border x:Name="Box" Width="20" Height="10" Background="#400000FF"/>
  <Border x:Name="Other" Width="20" Height="10" Background="#4000FF00"/>
</Grid>"##;

type Observed = Vec<(Entity, String, TransformSpec)>;

#[test]
fn render_transform_bridge_reads_back_assigned_transform() {
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "transforms.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "transforms.xaml".to_string(),
                        size: UVec2::new(64, 32),
                        ..default()
                    },
                    // Starts empty (no-op); filled after the scene exists so the
                    // one-shot apply isn't lost.
                    NoesisTransform::new(),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let observed_sys = Arc::clone(&observed);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut q: Query<&mut NoesisTransform>,
              mut changes: MessageReader<NoesisTransformChanged>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for mut t in &mut q {
                    *t = NoesisTransform::new()
                        .translate("Box", 50.0, 30.0)
                        .scale("Box", 2.0, 3.0)
                        .rotate("Box", 45.0);
                }
            }

            for ev in changes.read() {
                observed_sys
                    .lock()
                    .unwrap()
                    .push((ev.view, ev.name.clone(), ev.spec));
            }
        },
    );

    // Exit as soon as Box has reported its assigned RenderTransform back, not
    // after a padded frame count.
    let pred_observed = Arc::clone(&observed);
    let pred_view = Arc::clone(&view_entity);
    let read_back = run_until(&mut app, 240, move |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        pred_observed
            .lock()
            .unwrap()
            .iter()
            .rfind(|(e, name, _)| *e == view && name == "Box")
            .is_some_and(|(_, _, spec)| {
                spec.translate == [50.0, 30.0] && spec.scale == [2.0, 3.0] && spec.rotation == 45.0
            })
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed NoesisTransformChanged ---");
    for (e, name, spec) in &got {
        eprintln!("  {e:?} {name} = {spec:?}");
    }

    assert!(
        read_back,
        "Box never reported its assigned RenderTransform back within 240 frames; \
         observed {got:?}",
    );

    assert!(
        got.iter().all(|(_, name, _)| name != "Other"),
        "an un-transformed element must never emit NoesisTransformChanged",
    );

    let latest_box = got
        .iter()
        .rfind(|(e, name, _)| *e == view && name == "Box")
        .map(|(_, _, spec)| *spec)
        .expect("Box should report its assigned RenderTransform back");

    assert_eq!(
        latest_box.translate,
        [50.0, 30.0],
        "translate should round-trip through the element's live RenderTransform",
    );
    assert_eq!(latest_box.scale, [2.0, 3.0], "scale should round-trip");
    assert_eq!(latest_box.rotation, 45.0, "rotation should round-trip");
}
