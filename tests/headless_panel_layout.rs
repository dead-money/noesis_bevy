//! ECS-UI integration proof for F6: a per-name *write* bridge (here
//! [`NoesisLayout`]) placed on a mounted [`UiPanel`] entity resolves `x:Name`s
//! inside the panel's own fragment namescope, the same way `NoesisClickWatch`
//! does (F4). Before F6 the bridge silently no-op'd on a panel entity (the panel
//! isn't a `scene`), so geometry / layout / focus / transform panels couldn't be
//! split out of the host scene.
//!
//! The effect is observed spatially through the (working) F4 click path: the LEFT
//! panel's full-bleed button is pushed into a corner by a `NoesisLayout` margin,
//! so a click at the left half's center MISSES it; the RIGHT panel's button has no
//! margin and is the positive control (its click must land). If F6 were broken the
//! margin wouldn't apply, the left button would stay full-bleed, and the left click
//! would hit, so the assertion (only the right fired) fails before the fix.
//!
//! One `#[test]` per file (thread-affine Noesis runtime, one app per process).

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::input::{NoesisInputEvent, NoesisInputQueue};
use noesis_bevy::routed_events::MouseButton;
use noesis_bevy::{
    ClickWatchEntry, NoesisCamera, NoesisClickWatch, NoesisLayout, NoesisPanelAppExt, NoesisView,
    UiClicked, UiPanel, XamlRegistry,
};

mod common;
use common::{headless_app, run_until};

#[allow(dead_code)]
#[path = "../examples/ecs_ui.rs"]
mod ecs_ui;
use ecs_ui::Health;

// Host: two side-by-side full-bleed slots. SlotL covers x 0..32, SlotR x 32..64.
const HOST_XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml" Width="64" Height="32">
  <Grid.ColumnDefinitions><ColumnDefinition Width="*"/><ColumnDefinition Width="*"/></Grid.ColumnDefinitions>
  <Grid x:Name="SlotL" Grid.Column="0"/>
  <Grid x:Name="SlotR" Grid.Column="1"/>
</Grid>"##;

// Panel fragment: one full-bleed Button named in the fragment's OWN namescope.
const FRAG_XAML: &str = r##"<Button xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      x:Name="PanelBtn" HorizontalAlignment="Stretch" VerticalAlignment="Stretch" Content="X"/>"##;

// Left click fires first (25/27), then the right positive-control click (31/33).
// The right click landing is the terminal condition; the left click has already
// had its full chance to (wrongly) fire by then.
const PRESS_L: usize = 25;
const RELEASE_L: usize = 27;
const PRESS_R: usize = 31;
const RELEASE_R: usize = 33;

#[test]
fn layout_write_on_panel_entity_resolves_fragment_internal_name() {
    // event_target of every observed UiClicked.
    let observed: Arc<Mutex<Vec<Entity>>> = Arc::new(Mutex::new(Vec::new()));
    // (p1 = left, p2 = right)
    let ids: Arc<Mutex<Option<(Entity, Entity)>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();
    app.add_noesis_panel_field::<Health>();

    let obs = Arc::clone(&observed);
    app.add_observer(move |on: On<UiClicked>| {
        obs.lock().unwrap().push(on.event_target());
    });

    let ids_startup = Arc::clone(&ids);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "host.xaml".to_string(),
                Arc::new(HOST_XAML.as_bytes().to_vec()),
            );
            reg.insert(
                "frag.xaml".to_string(),
                Arc::new(FRAG_XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "host.xaml".to_string(),
                        size: UVec2::new(64, 32),
                        ..default()
                    },
                ))
                .id();
            // LEFT panel: a NoesisLayout margin pushes its fragment button into the
            // top-left corner of the left slot (8x8 at 0,0), away from the left
            // half's click point (16,16). This is the F6 path under test.
            let p1 = commands
                .spawn((
                    UiPanel::new("frag.xaml").mount_into(view, "SlotL"),
                    Health(100.0),
                    NoesisClickWatch::from_entries([ClickWatchEntry::new("PanelBtn")]),
                    NoesisLayout::new().margin("PanelBtn", [0.0, 0.0, 24.0, 24.0]),
                ))
                .id();
            // RIGHT panel: no margin; full-bleed button is the positive control.
            let p2 = commands
                .spawn((
                    UiPanel::new("frag.xaml").mount_into(view, "SlotR"),
                    Health(100.0),
                    NoesisClickWatch::from_entries([ClickWatchEntry::new("PanelBtn")]),
                ))
                .id();
            *ids_startup.lock().unwrap() = Some((p1, p2));
        },
    );

    app.add_systems(
        Update,
        move |mut frame: Local<usize>, mut input: ResMut<NoesisInputQueue>| {
            *frame += 1;
            // Click the LEFT half's center (16,16): hits only if the button is still
            // full-bleed there, i.e. if the F6 margin did NOT apply.
            if *frame == PRESS_L {
                input.push(NoesisInputEvent::MouseMove { x: 16, y: 16 });
                input.push(NoesisInputEvent::MouseButton {
                    down: true,
                    x: 16,
                    y: 16,
                    button: MouseButton::Left,
                });
            }
            if *frame == RELEASE_L {
                input.push(NoesisInputEvent::MouseButton {
                    down: false,
                    x: 16,
                    y: 16,
                    button: MouseButton::Left,
                });
            }
            // Click the RIGHT half's center (48,16): the positive control, always hits.
            if *frame == PRESS_R {
                input.push(NoesisInputEvent::MouseMove { x: 48, y: 16 });
                input.push(NoesisInputEvent::MouseButton {
                    down: true,
                    x: 48,
                    y: 16,
                    button: MouseButton::Left,
                });
            }
            if *frame == RELEASE_R {
                input.push(NoesisInputEvent::MouseButton {
                    down: false,
                    x: 48,
                    y: 16,
                    button: MouseButton::Left,
                });
            }
        },
    );

    // Exit once the right (positive-control) button has fired. The left click at
    // frames 25/27 has fully dispatched before the right press at 31, so if the
    // left button were going to (wrongly) fire it would already be recorded.
    let pred_obs = Arc::clone(&observed);
    let pred_ids = Arc::clone(&ids);
    let right_fired = run_until(&mut app, 160, move |_app| {
        let Some((_p1, p2)) = *pred_ids.lock().unwrap() else {
            return false;
        };
        pred_obs.lock().unwrap().contains(&p2)
    });

    let (p1, p2) = ids.lock().unwrap().expect("ids captured");
    let got = observed.lock().unwrap().clone();
    eprintln!(
        "--- observed UiClicked event_targets: {got:?}; p1(left)={p1:?} p2(right)={p2:?} ---"
    );

    // Positive control: the right (un-margined) button was hit, proving click
    // injection and the panel-entity click watch both work.
    assert!(
        right_fired,
        "positive control failed: the right panel's full-bleed button was not clicked within 160 frames; observed {got:?}",
    );
    assert!(
        got.contains(&p2),
        "positive control failed: the right panel's full-bleed button was not clicked; observed {got:?}",
    );
    // F6: the left button's NoesisLayout margin resolved against the fragment and
    // moved it out from under (16,16), so that click missed. Before F6 the margin
    // no-op'd, the button stayed full-bleed, and the left click would have hit.
    assert!(
        !got.contains(&p1),
        "left panel fired: its NoesisLayout margin did not reach the fragment button \
         (F6 fragment resolution missing), so it stayed full-bleed and the click hit; observed {got:?}",
    );
}
