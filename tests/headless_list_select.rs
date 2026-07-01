//! Control-side selection → ECS, the half `headless_list_query` cannot cover.
//!
//! The "currency is selection" contract says: when the user selects a row in the
//! live `ListBox`, the bridge marks that row's entity [`Selected`] and emits a
//! [`NoesisListSelection`]. We drive the closest faithful headless proxy for a row
//! click, setting the `ListBox`'s `SelectedIndex` through the [`NoesisDp`] bridge
//! (a real DP write on the actual control), and assert the bridge observes it.
//!
//! It does NOT cover the literal mouse-down hit-test (a `ListBoxItem` consuming a
//! pointer event); that path is `row_click_subs → UiClicked`, tested elsewhere.
//!
//! ## Regression guard for the control→bridge selection path
//! The bridge reads selection straight off the bound `ListBox` (`selected_item` /
//! `set_selected_index`): the control's own selection is the single source of
//! truth, so a control-side `SelectedIndex` write reaches `poll_selection` and
//! marks the row `Selected`. An earlier build instead observed a *fabricated*
//! `CollectionView` (the runtime's `GetView()` returns `new CollectionView(list)`
//! for an unhosted, code-built `CollectionViewSource`), which is **not** the live
//! `ListBox`'s default view, so control selection never arrived; this test guards
//! against regressing to that.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    DpKind, ListedIn, NoesisCamera, NoesisDp, NoesisListAppExt, NoesisListSelection, NoesisPlugin,
    NoesisView, NoesisViewModel, Selected, UiList, XamlRegistry,
};

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
const CAPTURE_AT: usize = 30;
const EXIT_AT: usize = 40;

#[test]
fn control_selection_marks_selected_and_emits_message() {
    noesis_license_from_env();

    let entities: Arc<Mutex<Option<(Entity, Entity, Entity)>>> = Arc::new(Mutex::new(None));
    // What the bridge marked Selected after we drove the control's SelectedIndex.
    let selected_after: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));
    let sel_msgs: Arc<Mutex<Vec<Option<Entity>>>> = Arc::new(Mutex::new(Vec::new()));

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
              selected_q: Query<Entity, With<Selected>>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;
            for ev in sel.read() {
                sel_msgs_sys.lock().unwrap().push(ev.selected);
            }

            // Drive the live ListBox's SelectedIndex to row 2 (C), the faithful
            // headless proxy for a user picking that row. The DP bridge targets the
            // list's view (where the scene lives), not the list entity.
            if *frame == SELECT_AT {
                if let Ok(list) = lists.single() {
                    commands.entity(list.view).insert(
                        NoesisDp::new().set_i32("Inv", "SelectedIndex", 2).watch(
                            "Inv",
                            "SelectedIndex",
                            DpKind::I32,
                        ),
                    );
                }
            }

            if *frame == CAPTURE_AT {
                *selected_after_sys.lock().unwrap() = selected_q.iter().next();
            }
            if *frame >= EXIT_AT {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let (_a, _b, c) = entities.lock().unwrap().expect("rows spawned");
    let selected = *selected_after.lock().unwrap();
    let msgs = sel_msgs.lock().unwrap().clone();

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

fn noesis_license_from_env() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
}
