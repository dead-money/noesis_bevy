//! Primitive 3 (events = observers), named-element half: a watched `Button`
//! click surfaces as a [`UiClicked`] `EntityEvent` that a global observer
//! receives, recovering the panel/view entity via `On::event_target()`.
//!
//! Drives a real headless [`NoesisPlugin`] app: injects a left mouse
//! down-then-up over a full-bleed `Button` watched by [`NoesisClickWatch`], and
//! asserts the observer saw a `UiClicked` whose target IS the view entity (the
//! default target for a named element).

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::input::{NoesisInputEvent, NoesisInputQueue};
use noesis_bevy::routed_events::MouseButton;
use noesis_bevy::{NoesisCamera, NoesisClickWatch, NoesisView, UiClicked, XamlRegistry};

use crate::common::{headless_app, run_until};

// 64x32 grid fully covered by a hit-testable Button.
const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <Button x:Name="Go" Content="Go"
          HorizontalAlignment="Stretch" VerticalAlignment="Stretch"/>
</Grid>"##;

const PRESS_AT: usize = 14;
const RELEASE_AT: usize = 16;

/// What an observer captured off a `UiClicked`: (target entity, view, name).
type Observed = Vec<(Entity, Entity, String)>;

#[test]
fn named_button_click_triggers_uiclicked_targeting_the_view() {
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    // Global observer: the push-based half of Primitive 3.
    let observed_obs = Arc::clone(&observed);
    app.add_observer(move |on: On<UiClicked>| {
        observed_obs
            .lock()
            .unwrap()
            .push((on.event_target(), on.view, on.name.clone()));
    });

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert("click.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "click.xaml".to_string(),
                        size: UVec2::new(64, 32),
                        ..default()
                    },
                    NoesisClickWatch::new(["Go"]),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    app.add_systems(
        Update,
        move |mut frame: Local<usize>, mut input: ResMut<NoesisInputQueue>| {
            *frame += 1;
            // Press inside the button, then release inside -> BaseButton::Click.
            if *frame == PRESS_AT {
                input.push(NoesisInputEvent::MouseMove { x: 32, y: 16 });
                input.push(NoesisInputEvent::MouseButton {
                    down: true,
                    x: 32,
                    y: 16,
                    button: MouseButton::Left,
                });
            }
            if *frame == RELEASE_AT {
                input.push(NoesisInputEvent::MouseButton {
                    down: false,
                    x: 32,
                    y: 16,
                    button: MouseButton::Left,
                });
            }
        },
    );

    // Exit as soon as the observer sees the "Go" button's click targeting the view.
    let pred_obs = Arc::clone(&observed);
    let pred_view = Arc::clone(&view_entity);
    let fired = run_until(&mut app, 120, move |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        pred_obs
            .lock()
            .unwrap()
            .iter()
            .any(|(target, _, name)| *target == view && name == "Go")
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed UiClicked ---");
    for (target, v, name) in &got {
        eprintln!("  target={target:?} view={v:?} name={name}");
    }

    assert!(
        fired,
        "observer never received a UiClicked targeting the view for button 'Go' \
         within 120 frames; observed {got:?}",
    );
    let hit = got
        .iter()
        .find(|(target, _, name)| *target == view && name == "Go")
        .expect("observer should have received a UiClicked targeting the view for button 'Go'");
    assert_eq!(hit.1, view, "UiClicked.view should be the originating view");
}
