//! End-to-end test of Primitive 2 (**list = query**) through the Bevy app.
//!
//! A host [`NoesisView`] scene carries a `ListBox` (`x:Name="Inv"`). Rows are
//! plain entities: each carries a `Row` component (`{Binding label}` /
//! `{Binding weight}`) and a [`ListedIn`] membership pointing at the view. A
//! [`UiList`] on the view binds the reconciled `ObservableCollection` to the
//! control, ordered by `weight`.
//!
//! Properties under test (all via the minimal [`NoesisListOps`] tally + the
//! [`Selected`] marker, never reading a "reset"):
//!   * **Add.** Spawning rows realizes them (an `adds` op), no clears.
//!   * **Update in place.** Mutating one row's *non-order* field produces an
//!     an `updates`-only op (no `adds`/`removes`/`moves`), proving the surviving
//!     row's existing instance was written, not re-created (no Reset).
//!   * **Reorder via Move.** Flipping the sort relocates rows with `moves` ops and
//!     **keeps the selected row selected**: currency rides the moved container.
//!   * **Remove.** Despawning a row drops it (`removes` op) without disturbing the
//!     rest or the selection.
//!   * **Default currency is NOT reported as a selection.** A fresh
//!     `ICollectionView` starts with the first row current, but the bridge adopts
//!     that baseline silently; it must not mark [`Selected`] or emit a
//!     [`NoesisListSelection`] before any genuine change. This test asserts no
//!     auto-selection and no spurious message. (NB: the binding observes its *own*
//!     `CollectionView`, a separate object from the live `ListBox`'s default view,
//!     so a real control-side row click does not reach this currency channel today;
//!     that goes through the `row_click_subs -> UiClicked` path. The
//!     control-to-currency link is covered, `#[ignore]`-d, in `headless_list_select`.)
//!   * **Currency is selection (ECS → UI).** Setting [`Selected`] from the app
//!     drives the current item; the marker survives the reorder, proving no Reset.
//!     App-driven selection is the *cause*, not an effect, so it emits **no**
//!     [`NoesisListSelection`], asserted via the message stream.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    ListedIn, NoesisCamera, NoesisListAppExt, NoesisListOps, NoesisListSelection, NoesisView,
    NoesisViewModel, Selected, UiList, XamlRegistry,
};

mod common;
use common::{headless_app, run_until};

// Host scene: a ListBox the rows bind into. An ItemTemplate binds the row's
// `label` so realization is realistic; the reconcile/currency assertions hold at
// the collection level regardless.
const HOST_XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="256" Height="256">
  <ListBox x:Name="Inv">
    <ListBox.ItemTemplate>
      <DataTemplate>
        <TextBlock Text="{Binding label}"/>
      </DataTemplate>
    </ListBox.ItemTemplate>
  </ListBox>
</Grid>"##;

/// A list row: `label` (string field) + `weight` (i32 sort key). Field order
/// fixes the property indices: `weight` is index 1, the sort key below.
#[derive(Component, NoesisViewModel)]
struct Row {
    label: String,
    weight: i32,
}

// Stimulus/capture timings. Each stage settles a few frames before the next, so
// an op raised by one action is observed before the following action fires.
// These sequence the scenario; the run's exit is the assert-worthy predicate
// below (all ops seen + selection survived), not a fixed frame count.
const CAPTURE_DEFAULT_AT: usize = 14;
const UPDATE_AT: usize = 18;
const SELECT_AT: usize = 28;
const REORDER_AT: usize = 38;
const REMOVE_AT: usize = 48;

#[derive(Default)]
struct OpFlags {
    saw_adds: bool,
    saw_update_only: bool,
    saw_moves: bool,
    saw_removes: bool,
    // Set once the pre-selection default-currency snapshot has been taken, so the
    // exit predicate knows that stage is behind us.
    default_captured: bool,
}

#[test]
fn list_reconciles_minimal_ops_and_keeps_selection() {
    let entities: Arc<Mutex<Option<(Entity, Entity, Entity)>>> = Arc::new(Mutex::new(None));
    let flags: Arc<Mutex<OpFlags>> = Arc::new(Mutex::new(OpFlags::default()));
    // Who the bridge auto-selected from default currency, before the app touches it.
    let default_selected: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));
    let sel_after_select: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));
    let sel_after_reorder: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));
    // Latest selection, refreshed every frame; read by the exit predicate.
    let final_selected: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));
    // UI-originated selection messages (app-driven selection emits none).
    let ui_sel_msgs: Arc<Mutex<Vec<Option<Entity>>>> = Arc::new(Mutex::new(Vec::new()));

    let mut app = headless_app();
    app.add_noesis_list::<Row>();

    let entities_startup = Arc::clone(&entities);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "host.xaml".to_string(),
                Arc::new(HOST_XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "host.xaml".to_string(),
                        size: UVec2::new(256, 256),
                        ..default()
                    },
                    // Order rows by weight, ascending: A(1), B(2), C(3).
                    UiList::new("Inv").sorted_by(1, false),
                ))
                .id();

            let a = commands
                .spawn((
                    Row {
                        label: "A".into(),
                        weight: 1,
                    },
                    ListedIn(view),
                ))
                .id();
            let b = commands
                .spawn((
                    Row {
                        label: "B".into(),
                        weight: 2,
                    },
                    ListedIn(view),
                ))
                .id();
            let c = commands
                .spawn((
                    Row {
                        label: "C".into(),
                        weight: 3,
                    },
                    ListedIn(view),
                ))
                .id();
            *entities_startup.lock().unwrap() = Some((a, b, c));
        },
    );

    let flags_sys = Arc::clone(&flags);
    let ui_sel_sys = Arc::clone(&ui_sel_msgs);
    let entities_sys = Arc::clone(&entities);
    let default_selected_sys = Arc::clone(&default_selected);
    let sel_after_select_sys = Arc::clone(&sel_after_select);
    let sel_after_reorder_sys = Arc::clone(&sel_after_reorder);
    let final_selected_sys = Arc::clone(&final_selected);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut commands: Commands,
              mut ops: MessageReader<NoesisListOps>,
              mut sel: MessageReader<NoesisListSelection>,
              mut lists: Query<&mut UiList>,
              mut rows: Query<&mut Row>,
              selected_q: Query<Entity, With<Selected>>| {
            *frame += 1;
            let (a, _b, c) = entities_sys.lock().unwrap().expect("rows spawned");

            {
                let mut f = flags_sys.lock().unwrap();
                for ev in ops.read() {
                    if ev.adds > 0 {
                        f.saw_adds = true;
                    }
                    if ev.moves > 0 {
                        f.saw_moves = true;
                    }
                    if ev.removes > 0 {
                        f.saw_removes = true;
                    }
                    if ev.updates > 0 && ev.adds == 0 && ev.removes == 0 && ev.moves == 0 {
                        f.saw_update_only = true;
                    }
                }
            }
            for ev in sel.read() {
                ui_sel_sys.lock().unwrap().push(ev.selected);
            }

            // Latest selection, always current for the exit predicate.
            *final_selected_sys.lock().unwrap() = selected_q.iter().next();

            // Default currency must NOT auto-select: before the app sets any
            // Selected of its own, nothing should be marked.
            if *frame == CAPTURE_DEFAULT_AT {
                *default_selected_sys.lock().unwrap() = selected_q.iter().next();
                flags_sys.lock().unwrap().default_captured = true;
            }

            // Mutate ONE row's non-order field: expect an updates-only op.
            if *frame == UPDATE_AT {
                if let Ok(mut row) = rows.get_mut(a) {
                    row.label = "AA".into();
                }
            }

            // App-driven selection: select C (currency is selection).
            if *frame == SELECT_AT {
                for e in &selected_q {
                    commands.entity(e).remove::<Selected>();
                }
                commands.entity(c).insert(Selected);
            }

            // A few frames later, record who is selected.
            if *frame == SELECT_AT + 6 {
                *sel_after_select_sys.lock().unwrap() = selected_q.iter().next();
            }

            // Flip the sort: A,B,C -> C,B,A. Selection must survive (Move).
            if *frame == REORDER_AT {
                if let Ok(mut list) = lists.single_mut() {
                    list.sort = Some(noesis_bevy::ListSort {
                        field: 1,
                        descending: true,
                    });
                }
            }
            if *frame == REORDER_AT + 6 {
                *sel_after_reorder_sys.lock().unwrap() = selected_q.iter().next();
            }

            // Remove a row (despawn): expect a removes op.
            if *frame == REMOVE_AT {
                if let Some((_a, b, _c)) = *entities_sys.lock().unwrap() {
                    commands.entity(b).despawn();
                }
            }
        },
    );

    // Exit once the scenario has fully played out: every reconcile op observed
    // and the app-driven selection has survived add/update/reorder/remove.
    let pred_flags = Arc::clone(&flags);
    let pred_final = Arc::clone(&final_selected);
    let pred_entities = Arc::clone(&entities);
    let completed = run_until(&mut app, 240, move |_app| {
        let Some((_a, _b, c)) = *pred_entities.lock().unwrap() else {
            return false;
        };
        let f = pred_flags.lock().unwrap();
        f.default_captured
            && f.saw_adds
            && f.saw_update_only
            && f.saw_moves
            && f.saw_removes
            && *pred_final.lock().unwrap() == Some(c)
    });

    let (_a, _b, c) = entities.lock().unwrap().expect("rows spawned");
    let f = flags.lock().unwrap();
    let default_sel = *default_selected.lock().unwrap();
    let ui_msgs = ui_sel_msgs.lock().unwrap().clone();
    let after_select = *sel_after_select.lock().unwrap();
    let after_reorder = *sel_after_reorder.lock().unwrap();
    let final_sel = *final_selected.lock().unwrap();

    assert!(
        completed,
        "scenario never reached its terminal state (add/update/move/remove + \
         surviving selection) within 240 frames",
    );
    assert!(f.saw_adds, "rows never realized (no adds op observed)");
    assert!(
        f.saw_update_only,
        "mutating one row's field did not produce an updates-only op (in-place \
         update with no Reset)",
    );
    assert!(
        f.saw_moves,
        "flipping the sort did not reorder via Move ops",
    );
    assert!(f.saw_removes, "despawning a row produced no removes op");

    // Default currency is adopted silently: before the app touches Selected, nothing
    // is marked and no UI selection message has been emitted; the unsolicited
    // first-frame auto-select is suppressed.
    assert_eq!(
        default_sel, None,
        "a fresh list auto-selected its first row — the default current item must \
         not be reported as a UI selection",
    );
    assert_eq!(
        ui_msgs,
        Vec::<Option<Entity>>::new(),
        "expected no UI selection messages (default currency suppressed; app-driven \
         Selected emits none). got {ui_msgs:?}",
    );

    assert_eq!(
        after_select,
        Some(c),
        "app-set Selected(C) did not stick (currency did not become C)",
    );
    assert_eq!(
        after_reorder,
        Some(c),
        "selection was lost across the reorder — Move did not preserve currency \
         (a Reset would have cleared it)",
    );
    assert_eq!(
        final_sel,
        Some(c),
        "selection did not survive to the end of the run",
    );
}
