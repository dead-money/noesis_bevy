//! Regression for the multi-row-type clobber (audit P0.3).
//!
//! Two [`NoesisView`]s each own a `ListBox` bound by a [`UiList`], but the two
//! lists carry *different* row component types: view 1 lists `RowA`, view 2 lists
//! `RowB`. Both types are registered with [`NoesisListAppExt::add_noesis_list`], so
//! each gets its own `diff_list::<T>` system iterating **every** list. The bug: a
//! type with no rows in a given list still overwrote that list's `schema` / `rows`
//! / `selected`, so whichever `diff_list` ran last emptied the other type's list
//! (and could freeze the row class with the wrong field layout).
//!
//! After the fix only the owning type writes a slot, so both lists realize their
//! rows regardless of scheduler order.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    ListedIn, NoesisCamera, NoesisListAppExt, NoesisListOps, NoesisPlugin, NoesisView,
    NoesisViewModel, UiList, XamlRegistry,
};

const HOST_XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="256" Height="256">
  <ListBox x:Name="List">
    <ListBox.ItemTemplate>
      <DataTemplate>
        <TextBlock Text="{Binding label}"/>
      </DataTemplate>
    </ListBox.ItemTemplate>
  </ListBox>
</Grid>"##;

/// Row type for view 1.
#[derive(Component, NoesisViewModel)]
struct RowA {
    label: String,
}

/// Row type for view 2 (distinct field layout).
#[derive(Component, NoesisViewModel)]
struct RowB {
    name: String,
    weight: i32,
}

const EXIT_AT: usize = 48;

#[test]
fn two_row_types_do_not_clobber_each_others_lists() {
    noesis_license_from_env();

    let views: Arc<Mutex<Option<(Entity, Entity)>>> = Arc::new(Mutex::new(None));
    // Cumulative adds observed per view: proves each list realized its own rows.
    let adds_a: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
    let adds_b: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));

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
    app.add_noesis_list::<RowA>();
    app.add_noesis_list::<RowB>();

    let views_startup = Arc::clone(&views);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "host.xaml".to_string(),
                Arc::new(HOST_XAML.as_bytes().to_vec()),
            );

            let view_a = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "host.xaml".to_string(),
                        size: UVec2::new(256, 256),
                        ..default()
                    },
                    UiList::new("List"),
                ))
                .id();
            let view_b = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "host.xaml".to_string(),
                        size: UVec2::new(256, 256),
                        ..default()
                    },
                    UiList::new("List"),
                ))
                .id();

            commands.spawn((RowA { label: "a1".into() }, ListedIn(view_a)));
            commands.spawn((RowA { label: "a2".into() }, ListedIn(view_a)));
            commands.spawn((
                RowB {
                    name: "b1".into(),
                    weight: 1,
                },
                ListedIn(view_b),
            ));
            commands.spawn((
                RowB {
                    name: "b2".into(),
                    weight: 2,
                },
                ListedIn(view_b),
            ));
            commands.spawn((
                RowB {
                    name: "b3".into(),
                    weight: 3,
                },
                ListedIn(view_b),
            ));

            *views_startup.lock().unwrap() = Some((view_a, view_b));
        },
    );

    let views_sys = Arc::clone(&views);
    let adds_a_sys = Arc::clone(&adds_a);
    let adds_b_sys = Arc::clone(&adds_b);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut ops: MessageReader<NoesisListOps>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;
            let (view_a, view_b) = views_sys.lock().unwrap().expect("views spawned");
            for ev in ops.read() {
                if ev.view == view_a {
                    *adds_a_sys.lock().unwrap() += ev.adds as u32;
                } else if ev.view == view_b {
                    *adds_b_sys.lock().unwrap() += ev.adds as u32;
                }
            }
            if *frame >= EXIT_AT {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let realized_a = *adds_a.lock().unwrap();
    let realized_b = *adds_b.lock().unwrap();

    assert_eq!(
        realized_a, 2,
        "view A's RowA list did not realize its 2 rows (clobbered by RowB's \
         diff_list?); got {realized_a} adds",
    );
    assert_eq!(
        realized_b, 3,
        "view B's RowB list did not realize its 3 rows (clobbered by RowA's \
         diff_list?); got {realized_b} adds",
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
