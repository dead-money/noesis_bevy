//! Control-side selection to ECS, the half `headless_list_query` does not cover.
//!
//! When the user selects a row in the live `ListBox`, the bridge marks that row's
//! entity [`Selected`] and emits a [`NoesisListSelection`]. The headless proxy for
//! a row click is a `SelectedIndex` write on the control through the [`NoesisDp`]
//! bridge; the test asserts the bridge observes it.
//!
//! The bridge must read selection off the bound `ListBox` itself. A code-built
//! `CollectionViewSource`'s `GetView()` returns a fresh `CollectionView`, not the
//! control's default view, so observing that view never sees control-side
//! selection.
//!
//! The mouse hit-test path (`row_click_subs` to `UiClicked`) is tested elsewhere.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    DpKind, ListedIn, NoesisCamera, NoesisDp, NoesisListAppExt, NoesisListSelection, NoesisView,
    NoesisViewModel, Selected, UiList, XamlRegistry,
};

use crate::common::{headless_app, run_until};

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

#[derive(Component, NoesisViewModel)]
struct Row {
    label: String,
    weight: i32,
}

const SELECT_AT: usize = 16;

#[test]
fn control_selection_marks_selected_and_emits_message() {
    let entities: Arc<Mutex<Option<(Entity, Entity, Entity)>>> = Arc::new(Mutex::new(None));
    // Latest Selected the bridge marked after we drove the control's SelectedIndex.
    let selected_after: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));
    let sel_msgs: Arc<Mutex<Vec<Option<Entity>>>> = Arc::new(Mutex::new(Vec::new()));

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
                ))
                .id();
            let list = commands
                .spawn(UiList::new(view, "Inv").sorted_by(1, false)) // A(1), B(2), C(3)
                .id();
            let a = commands
                .spawn((
                    Row {
                        label: "A".into(),
                        weight: 1,
                    },
                    ListedIn(list),
                ))
                .id();
            let b = commands
                .spawn((
                    Row {
                        label: "B".into(),
                        weight: 2,
                    },
                    ListedIn(list),
                ))
                .id();
            let c = commands
                .spawn((
                    Row {
                        label: "C".into(),
                        weight: 3,
                    },
                    ListedIn(list),
                ))
                .id();
            *entities_startup.lock().unwrap() = Some((a, b, c));
        },
    );

    let selected_after_sys = Arc::clone(&selected_after);
    let sel_msgs_sys = Arc::clone(&sel_msgs);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut commands: Commands,
              lists: Query<&UiList>,
              mut sel: MessageReader<NoesisListSelection>,
              selected_q: Query<Entity, With<Selected>>| {
            *frame += 1;
            for ev in sel.read() {
                sel_msgs_sys.lock().unwrap().push(ev.selected);
            }

            *selected_after_sys.lock().unwrap() = selected_q.iter().next();

            // Select row 2 (C). The DP bridge targets the list's view (where the
            // scene lives), not the list entity.
            if *frame == SELECT_AT
                && let Ok(list) = lists.single()
            {
                commands.entity(list.view).insert(
                    NoesisDp::new().set_i32("Inv", "SelectedIndex", 2).watch(
                        "Inv",
                        "SelectedIndex",
                        DpKind::I32,
                    ),
                );
            }
        },
    );

    let pred_entities = Arc::clone(&entities);
    let pred_selected = Arc::clone(&selected_after);
    let pred_msgs = Arc::clone(&sel_msgs);
    let reached = run_until(&mut app, 160, move |_app| {
        let Some((_a, _b, c)) = *pred_entities.lock().unwrap() else {
            return false;
        };
        *pred_selected.lock().unwrap() == Some(c) && pred_msgs.lock().unwrap().contains(&Some(c))
    });

    let (_a, _b, c) = entities.lock().unwrap().expect("rows spawned");
    let selected = *selected_after.lock().unwrap();
    let msgs = sel_msgs.lock().unwrap().clone();

    assert!(
        reached,
        "control-side SelectedIndex write never reached the bridge (C marked \
         Selected + NoesisListSelection for C) within 160 frames; selected \
         {selected:?} msgs {msgs:?}",
    );
    assert_eq!(
        selected,
        Some(c),
        "selecting row 2 (C) in the ListBox did not mark its entity Selected — the \
         control's selection did not reach the bridge",
    );
    assert!(
        msgs.contains(&Some(c)),
        "selecting row 2 (C) emitted no NoesisListSelection for C; got {msgs:?}",
    );
}
