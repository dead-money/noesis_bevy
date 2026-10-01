//! ECS-UI integration proof: a [`NoesisEventWatch`] placed on a mounted
//! [`UiPanel`] entity resolves `x:Name`s inside the panel's *own* fragment
//! namescope and fires a [`UiRoutedEvent`] that targets the panel entity,
//! carrying the host as its `view` — the generic-routed-event twin of
//! `headless_panel_click.rs`. Two instances of the same fragment XAML stay
//! isolated: moving the mouse over one panel's element never fires the other's
//! watch. Watches `MouseMove` (`MouseEnter` generation is not exercised by the
//! headless input pump; move is the hover primitive that provably fires).
//!
//! This is what makes fragment-internal hover (e.g. a palette button's
//! mouse-over preview) reachable from Rust: before, a `NoesisEventWatch` on a
//! panel entity was silently ignored (the panel isn't a `scene`).
//!
//! One `#[test]` per file (thread-affine Noesis runtime, one app per process).

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::input::{NoesisInputEvent, NoesisInputQueue};
use noesis_bevy::{
    EventWatchEntry, NoesisCamera, NoesisEventWatch, NoesisPanelAppExt, NoesisView, RoutedEvent,
    UiPanel, UiRoutedEvent, XamlRegistry,
};

use crate::common::{headless_app, run_until};

use crate::ecs_ui::Health;

// Host scene: two side-by-side full-bleed mount slots. SlotL covers x 0..32,
// SlotR covers x 32..64 (both full height).
const HOST_XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml" Width="64" Height="32">
  <Grid.ColumnDefinitions><ColumnDefinition Width="*"/><ColumnDefinition Width="*"/></Grid.ColumnDefinitions>
  <Grid x:Name="SlotL" Grid.Column="0"/>
  <Grid x:Name="SlotR" Grid.Column="1"/>
</Grid>"##;

// Panel fragment: one full-bleed hit-testable element named in the fragment's
// OWN namescope. Both panels load this same XAML, so "PanelHot" exists twice,
// once per private namescope: the case a root-level FindName can't
// disambiguate.
const FRAG_XAML: &str = r##"<Border xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      x:Name="PanelHot" Background="#FF224488"/>"##;

// Panels mount + seal over the first frames; move the mouse in once they are
// live. Stimulus timing, not the exit condition.
const ENTER_AT: usize = 25;

#[test]
fn event_watch_on_panel_entity_resolves_fragment_internal_name() {
    // (event_target, view, name, event) for every observed UiRoutedEvent.
    #[allow(clippy::type_complexity)]
    let observed: Arc<Mutex<Vec<(Entity, Entity, String, RoutedEvent)>>> =
        Arc::new(Mutex::new(Vec::new()));
    // (view, p1, p2)
    let ids: Arc<Mutex<Option<(Entity, Entity, Entity)>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();
    // A registered bound field so each UiPanel actually builds + mounts; the
    // fragment element doesn't bind it, it just needs to be hit-testable.
    app.add_noesis_panel_field::<Health>();

    let obs = Arc::clone(&observed);
    app.add_observer(move |on: On<UiRoutedEvent>| {
        obs.lock()
            .unwrap()
            .push((on.event_target(), on.view, on.name.clone(), on.event));
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
            // Two instances of the SAME fragment. Each watches "PanelHot" on its
            // OWN panel entity (no explicit target, so default = the panel entity).
            let p1 = commands
                .spawn((
                    UiPanel::new("frag.xaml").mount_into(view, "SlotL"),
                    Health(100.0),
                    NoesisEventWatch::new([EventWatchEntry::new(
                        "PanelHot",
                        RoutedEvent::MouseMove,
                    )]),
                ))
                .id();
            let p2 = commands
                .spawn((
                    UiPanel::new("frag.xaml").mount_into(view, "SlotR"),
                    Health(100.0),
                    NoesisEventWatch::new([EventWatchEntry::new(
                        "PanelHot",
                        RoutedEvent::MouseMove,
                    )]),
                ))
                .id();
            *ids_startup.lock().unwrap() = Some((view, p1, p2));
        },
    );

    app.add_systems(
        Update,
        move |mut frame: Local<usize>, mut input: ResMut<NoesisInputQueue>| {
            *frame += 1;
            // Move onto the LEFT slot's element (center of the left half).
            if *frame == ENTER_AT {
                input.push(NoesisInputEvent::MouseMove { x: 16, y: 16 });
            }
        },
    );

    // Exit as soon as the left panel's fragment-internal element has fired.
    let pred_obs = Arc::clone(&observed);
    let pred_ids = Arc::clone(&ids);
    let entered = run_until(&mut app, 120, move |_app| {
        let Some((view, p1, _p2)) = *pred_ids.lock().unwrap() else {
            return false;
        };
        pred_obs.lock().unwrap().iter().any(|(t, v, n, e)| {
            *t == p1 && *v == view && n == "PanelHot" && *e == RoutedEvent::MouseMove
        })
    });

    let (view, p1, p2) = ids.lock().unwrap().expect("ids captured");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed UiRoutedEvent: {got:?}; view={view:?} p1={p1:?} p2={p2:?} ---");

    // The fragment-internal element fired: a UiRoutedEvent targeting its panel
    // entity, carrying the host view. Before the fix, the watch on a panel
    // entity was silently ignored (the panel isn't a `scene`).
    assert!(
        entered,
        "expected a MouseMove UiRoutedEvent from the left panel's fragment element \
         targeting p1 with view {view:?}; observed {got:?}",
    );
    // Namescope isolation: the right panel's identically-named element never
    // fired (the pointer only ever moved over the left half).
    assert!(
        !got.iter().any(|(t, _, _, _)| *t == p2),
        "right panel p2 fired without being hovered (namescope cross-talk); observed {got:?}",
    );
}
