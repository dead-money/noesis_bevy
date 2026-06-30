//! ECS-UI integration proof — **Primitive 2 (list = query)** plus the per-row half
//! of **Primitive 3**: add / update-in-place / reorder-via-Move / remove all
//! surface as the minimal op tally (never a Reset), a real row click fires a
//! [`UiClicked`] targeting that row's *entity*, and selection round-trips through
//! the [`Selected`] marker — surviving a reorder. Driven on the [`ecs_ui`]
//! example's own `Item` row type and [`ecs_ui::on_row_click`] observer.
//!
//! One `#[test]` per file (thread-affine Noesis runtime → one app per process).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::input::{NoesisInputEvent, NoesisInputQueue};
use noesis_bevy::routed_events::MouseButton;
use noesis_bevy::{
    ListedIn, NoesisCamera, NoesisListAppExt, NoesisListOps, NoesisListSelection, NoesisPlugin,
    NoesisView, Selected, UiClicked, UiList, XamlRegistry,
};

#[allow(dead_code)]
#[path = "../examples/ecs_ui.rs"]
mod ecs_ui;

use ecs_ui::Item;

// A non-virtualizing ItemsControl whose rows are fixed-height, full-width,
// hit-testable Borders. Rows order by qty ascending; the top row spans y=[0,40].
// An ItemsControl (not a ListBox) lets each row's MouseLeftButtonUp bubble out as
// a per-row UiClicked instead of being swallowed by a Selector.
const LIST_XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="256" Height="256">
  <ItemsControl x:Name="Inventory">
    <ItemsControl.ItemTemplate>
      <DataTemplate>
        <Border Background="#FF404040" Height="40" Width="256">
          <TextBlock Text="{Binding name}"/>
        </Border>
      </DataTemplate>
    </ItemsControl.ItemTemplate>
  </ItemsControl>
</Grid>"##;

#[derive(Default)]
struct OpFlags {
    adds: bool,
    update_only: bool,
    moves: bool,
    removes: bool,
}

// A fresh ICollectionView starts with the FIRST row (A) current, so the bridge
// marks A Selected by default. We click the SECOND row (B) to prove the click
// actually drives selection — B is not the default.
const PRESS_AT: usize = 24; // click row B (y=60) after rows realize
const RELEASE_AT: usize = 26;
const CAPTURE_SEL_AT: usize = 32;
const UPDATE_AT: usize = 36; // mutate a non-order field -> update-only
const REORDER_AT: usize = 44; // flip sort -> Move ops; selection must survive
const CAPTURE_REORDER_AT: usize = 50;
const REMOVE_AT: usize = 54; // despawn -> removes op
const EXIT_AT: usize = 66;

#[test]
fn list_reconciles_minimally_and_row_click_selects() {
    noesis_license_from_env();

    let entities: Arc<Mutex<Option<(Entity, Entity, Entity)>>> = Arc::new(Mutex::new(None));
    let flags: Arc<Mutex<OpFlags>> = Arc::new(Mutex::new(OpFlags::default()));
    let row_clicks: Arc<Mutex<Vec<Entity>>> = Arc::new(Mutex::new(Vec::new()));
    let sel_after_click: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));
    let sel_after_reorder: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

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
    app.add_noesis_list::<Item>();

    // Capture row-targeted clicks, and drive selection with the EXAMPLE's own
    // observer (clicking a row sets Selected on that row entity).
    let clicks_obs = Arc::clone(&row_clicks);
    app.add_observer(move |on: On<UiClicked>, items: Query<&Item>| {
        if items.get(on.event_target()).is_ok() {
            clicks_obs.lock().unwrap().push(on.event_target());
        }
    });
    app.add_observer(ecs_ui::on_row_click);

    let entities_startup = Arc::clone(&entities);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "list.xaml".to_string(),
                Arc::new(LIST_XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "list.xaml".to_string(),
                        size: UVec2::new(256, 256),
                        ..default()
                    },
                    // Order rows by qty (property index 1), ascending: A(1),B(2),C(3).
                    UiList::new("Inventory", "EcsUiTest.Row").sorted_by(1, false),
                ))
                .id();
            let mk = |commands: &mut Commands, name: &str, qty: i32| {
                commands
                    .spawn((
                        Item {
                            name: name.to_string(),
                            qty,
                        },
                        ListedIn(view),
                    ))
                    .id()
            };
            let a = mk(&mut commands, "A", 1);
            let b = mk(&mut commands, "B", 2);
            let c = mk(&mut commands, "C", 3);
            *entities_startup.lock().unwrap() = Some((a, b, c));
        },
    );

    let flags_sys = Arc::clone(&flags);
    let entities_sys = Arc::clone(&entities);
    let sel_click_sys = Arc::clone(&sel_after_click);
    let sel_reorder_sys = Arc::clone(&sel_after_reorder);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut commands: Commands,
              mut input: ResMut<NoesisInputQueue>,
              mut ops: MessageReader<NoesisListOps>,
              mut sel_msgs: MessageReader<NoesisListSelection>,
              mut lists: Query<&mut UiList>,
              mut items: Query<&mut Item>,
              selected_q: Query<Entity, With<Selected>>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;
            let (_a, b, _c) = entities_sys.lock().unwrap().expect("rows spawned");

            for ev in ops.read() {
                let mut f = flags_sys.lock().unwrap();
                if ev.adds > 0 {
                    f.adds = true;
                }
                if ev.moves > 0 {
                    f.moves = true;
                }
                if ev.removes > 0 {
                    f.removes = true;
                }
                if ev.updates > 0 && ev.adds == 0 && ev.removes == 0 && ev.moves == 0 {
                    f.update_only = true;
                }
            }
            // Drain the UI-selection message stream (a real app would react here).
            for _ in sel_msgs.read() {}

            // Real click on the second row (B at y=60): a per-row UiClicked.
            if *frame == PRESS_AT {
                input.push(NoesisInputEvent::MouseMove { x: 100, y: 60 });
                input.push(NoesisInputEvent::MouseButton {
                    down: true,
                    x: 100,
                    y: 60,
                    button: MouseButton::Left,
                });
            }
            if *frame == RELEASE_AT {
                input.push(NoesisInputEvent::MouseButton {
                    down: false,
                    x: 100,
                    y: 60,
                    button: MouseButton::Left,
                });
            }
            if *frame == CAPTURE_SEL_AT {
                *sel_click_sys.lock().unwrap() = selected_q.iter().next();
            }
            // In-place field edit on a surviving row -> updates-only.
            if *frame == UPDATE_AT
                && let Ok(mut row) = items.get_mut(b)
            {
                row.name = "BB".into();
            }
            // Flip the sort: A,B,C -> C,B,A. Selection (B) must ride the Move.
            if *frame == REORDER_AT
                && let Ok(mut list) = lists.single_mut()
            {
                list.sort = Some(noesis_bevy::ListSort {
                    field: 1,
                    descending: true,
                });
            }
            if *frame == CAPTURE_REORDER_AT {
                *sel_reorder_sys.lock().unwrap() = selected_q.iter().next();
            }
            if *frame == REMOVE_AT {
                commands.entity(b).despawn();
            }
            if *frame >= EXIT_AT {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let (_a, b, _c) = entities.lock().unwrap().expect("rows spawned");
    let f = flags.lock().unwrap();
    let clicks = row_clicks.lock().unwrap().clone();
    let after_click = *sel_after_click.lock().unwrap();
    let after_reorder = *sel_after_reorder.lock().unwrap();
    eprintln!(
        "--- row clicks: {clicks:?}; sel@click={after_click:?} sel@reorder={after_reorder:?} ---"
    );

    // Reconcile: the minimal op shapes, never a Reset.
    assert!(f.adds, "rows never realized (no adds op)");
    assert!(
        f.update_only,
        "editing one row's field did not produce an updates-only op (in-place, no Reset)",
    );
    assert!(f.moves, "flipping the sort did not reorder via Move ops");
    assert!(f.removes, "despawning a row produced no removes op");

    // Per-row event: the click targeted row B's entity (no x:Name involved).
    assert!(
        clicks.contains(&b),
        "row click did not fire a UiClicked targeting the clicked row entity (B={b:?}); got {clicks:?}",
    );
    // Selection round-trip: the click moved selection off the default (A) onto B
    // (via the example's observer), and B stayed selected across the reorder —
    // currency rode the Move, no Reset.
    assert_eq!(
        after_click,
        Some(b),
        "row click did not land Selected on row B (read back via With<Selected>)",
    );
    assert_eq!(
        after_reorder,
        Some(b),
        "selection was lost across the reorder — Move did not preserve currency",
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
