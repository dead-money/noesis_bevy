//! The motivating case for **list = entity**: two [`UiList`]s bound to the *same*
//! [`NoesisView`] (two `ListBox`es in one scene) each realize their own rows.
//!
//! When `UiList` was a component on the view entity, a view could bind exactly one
//! list (one component of a type per entity), even though the render store was
//! already keyed `(view, name)`. Making the list its own entity lifts that cap: two
//! `UiList` entities, same `view`, different `x:Name`, populated from the same row
//! type via [`ListedIn`] pointing at each list entity.
//!
//! Drives both lists to steady state and asserts each realized exactly its own rows
//! (Left: 2, Right: 3) and the render state tracks two live bindings for the one
//! view.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    ListedIn, NoesisCamera, NoesisDiagnostics, NoesisListAppExt, NoesisListOps, NoesisView,
    NoesisViewModel, UiList, XamlRegistry,
};

use crate::common::{headless_app, run_until};

// One scene, two independent ListBoxes. Each list entity binds one of them by name.
const HOST_XAML: &str = r##"<StackPanel xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="256" Height="256">
  <ListBox x:Name="Left">
    <ListBox.ItemTemplate>
      <DataTemplate><TextBlock Text="{Binding label}"/></DataTemplate>
    </ListBox.ItemTemplate>
  </ListBox>
  <ListBox x:Name="Right">
    <ListBox.ItemTemplate>
      <DataTemplate><TextBlock Text="{Binding label}"/></DataTemplate>
    </ListBox.ItemTemplate>
  </ListBox>
</StackPanel>"##;

#[derive(Component, NoesisViewModel)]
struct Row {
    label: String,
    weight: i32,
}

#[test]
fn two_lists_in_one_view_each_realize_their_own_rows() {
    // Cumulative adds observed per list name; proves each ListBox realized its rows.
    let adds_left: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
    let adds_right: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
    let live_lists: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));

    let mut app = headless_app();
    app.add_noesis_list::<Row>();

    app.add_systems(
        Startup,
        |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "two_lists.xaml".to_string(),
                Arc::new(HOST_XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "two_lists.xaml".to_string(),
                        size: UVec2::new(256, 256),
                        ..default()
                    },
                ))
                .id();

            // Two list entities, one view, two different controls.
            let left = commands
                .spawn(UiList::new(view, "Left").sorted_by(1, false))
                .id();
            let right = commands
                .spawn(UiList::new(view, "Right").sorted_by(1, false))
                .id();

            for (label, weight) in [("L1", 1), ("L2", 2)] {
                commands.spawn((
                    Row {
                        label: label.into(),
                        weight,
                    },
                    ListedIn(left),
                ));
            }
            for (label, weight) in [("R1", 1), ("R2", 2), ("R3", 3)] {
                commands.spawn((
                    Row {
                        label: label.into(),
                        weight,
                    },
                    ListedIn(right),
                ));
            }
        },
    );

    let adds_left_sys = Arc::clone(&adds_left);
    let adds_right_sys = Arc::clone(&adds_right);
    let live_lists_sys = Arc::clone(&live_lists);
    app.add_systems(
        Update,
        move |mut ops: MessageReader<NoesisListOps>, diag: Res<NoesisDiagnostics>| {
            for ev in ops.read() {
                match ev.list.as_str() {
                    "Left" => *adds_left_sys.lock().unwrap() += ev.adds as u32,
                    "Right" => *adds_right_sys.lock().unwrap() += ev.adds as u32,
                    other => panic!("unexpected list name {other:?}"),
                }
            }
            *live_lists_sys.lock().unwrap() = diag.live_lists;
        },
    );

    // Exit once both lists realized all their own rows (Left: 2, Right: 3).
    let pred_left = Arc::clone(&adds_left);
    let pred_right = Arc::clone(&adds_right);
    let realized = run_until(&mut app, 160, move |_app| {
        *pred_left.lock().unwrap() == 2 && *pred_right.lock().unwrap() == 3
    });

    let realized_left = *adds_left.lock().unwrap();
    let realized_right = *adds_right.lock().unwrap();
    let lists = *live_lists.lock().unwrap();
    eprintln!(
        "--- two lists per view: left={realized_left} right={realized_right} live={lists} ---"
    );

    assert!(
        realized,
        "both lists never realized all their rows (Left expects 2, Right expects 3) \
         within 160 frames; got Left={realized_left} Right={realized_right}",
    );
    assert_eq!(realized_left, 2, "Left list did not realize its 2 rows");
    assert_eq!(realized_right, 3, "Right list did not realize its 3 rows");
    assert_eq!(
        lists, 2,
        "one view should own two live list bindings; got {lists}",
    );
}
