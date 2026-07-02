//! Integration test for the typed `ItemsSource` bridge (`noesis_bevy::items`),
//! run headless through the full `NoesisPlugin` pipeline.
//!
//! Sets three `i32` items `[10, 20, 30]` on a `ListBox` and drives selection to
//! index 1. Asserts the round-trip via `NoesisItemsCurrent`:
//!
//!   * `count == 3`: items reached the live control (unbound reads 0).
//!   * `selected_index == 1`: selection was applied (untouched default is -1).
//!   * `current == ItemValue::I32(20)`: typed unbox at index 1; rules out a
//!     string list, rules out the default position 0 (value 10).
//!
//! Theme-free XAML with no `ItemTemplate`; no font gate needed.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    ItemValue, NoesisCamera, NoesisItems, NoesisItemsCurrent, NoesisView, XamlRegistry,
};

mod common;
use common::{headless_app, run_until};

const VIEW_W: u32 = 120;
const VIEW_H: u32 = 80;

// wait for scene to be live; one-shot change-detection apply is lost before the control exists
const SET_AT_FRAME: usize = 12;

const XAML: &str = r##"<ListBox xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      x:Name="Ports" Width="120" Height="80"/>"##;

// (frame, entity, name, count, selected_index, current); frame retained to verify drive ordering
type Observed = Vec<(usize, Entity, String, usize, i32, Option<ItemValue>)>;

#[test]
fn typed_items_populate_select_and_read_back() {
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert("ports.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "ports.xaml".to_string(),
                        size: UVec2::new(VIEW_W, VIEW_H),
                        ..default()
                    },
                    // empty; filled at SET_AT_FRAME once the scene is live
                    NoesisItems::new(),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let observed_sys = Arc::clone(&observed);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut q: Query<&mut NoesisItems>,
              mut changes: MessageReader<NoesisItemsCurrent>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for mut items in &mut q {
                    *items = NoesisItems::new()
                        .with("Ports", [10, 20, 30])
                        .select("Ports", 1);
                }
            }

            for ev in changes.read() {
                observed_sys.lock().unwrap().push((
                    *frame,
                    ev.view,
                    ev.name.clone(),
                    ev.count,
                    ev.selected_index,
                    ev.current.clone(),
                ));
            }
        },
    );

    // Exit once the driven selection has round-tripped back: 3 items present,
    // index 1 current, unboxing to the typed i32 20.
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
            .rfind(|(_, e, name, _, _, _)| *e == view && name == "Ports")
            .is_some_and(|(_, _, _, count, idx, current)| {
                *count == 3 && *idx == 1 && *current == Some(ItemValue::I32(20))
            })
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed NoesisItemsCurrent ---");
    for (f, e, name, count, idx, current) in &got {
        eprintln!("  f{f} {e:?} {name} count={count} idx={idx} current={current:?}");
    }

    assert!(
        read_back,
        "typed items never converged (count 3 / index 1 / I32(20)) within 240 \
         frames; observed {got:?}",
    );

    let latest = got
        .iter()
        .rfind(|(_, e, name, _, _, _)| *e == view && name == "Ports");
    let (_, _, _, count, idx, current) = latest
        .expect("no NoesisItemsCurrent for the ListBox — typed items never bound / read back");

    assert_eq!(
        *count, 3,
        "the 3 typed i32 items should have reached the live ListBox (got count {count})",
    );
    assert_eq!(
        *idx, 1,
        "selection should have been driven to index 1 (default for an untouched \
         ListBox is -1); got {idx}",
    );
    assert_eq!(
        *current,
        Some(ItemValue::I32(20)),
        "the current item at index 1 should read back as the typed i32 20 \
         (default position 0 is 10; a string list would never unbox to I32); got {current:?}",
    );

    // Bluff-catch: a selected_index of 1 must never have surfaced before we drove
    // it (would mean the bridge fabricated selection rather than applying ours).
    assert!(
        got.iter()
            .all(|(f, _, _, _, sel, _)| *sel != 1 || *f >= SET_AT_FRAME),
        "selected_index 1 surfaced before the drive frame; got {got:?}",
    );
}
