//! Despawn-teardown regression for **Primitive 2 (list = query)**: despawning a
//! [`NoesisView`] that owns a [`UiList`] must reap that view's `ListBinding`, not
//! leak it.
//!
//! A `ListBinding` holds Noesis refcounted state in a strict drop order: the
//! `ObservableCollection` (releasing its refs to the row instances) → each realized
//! row `ClassInstance` (our `+1`) → the row-class `ClassRegistration` (which
//! unregisters the class, and must outlive every instance of it). That ordering is
//! one of the project's hard invariants ("drop order matters"); a regression
//! (class unregistered before its instances release) would be use-after-free-adjacent
//! and could ship silently because no test despawned a list-owning view.
//!
//! This drives a view with a `ListBox` + a few entity-rows until its binding is
//! live (`live_lists == 1`), despawns the view, and asserts the live-list count
//! drains back to 0; the `teardown_for` reap path ran in refcount order.
//!
//! Font-free XAML so the scene builds without a font folder.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    ListedIn, NoesisCamera, NoesisDiagnostics, NoesisListAppExt, NoesisPlugin, NoesisView,
    NoesisViewModel, UiList, XamlRegistry,
};

// A ListBox bound by the UiList. An ItemTemplate binds the row's `label`, so rows
// genuinely realize (each a live ClassInstance the binding must release on reap).
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

const CAPTURE_PRE_AT: usize = 20;
const DESPAWN_AT: usize = 21;
const CAPTURE_POST_AT: usize = 45;
const EXIT_AT: usize = 55;

#[test]
fn despawning_a_list_owning_view_reaps_its_binding() {
    noesis_license_from_env();

    let pre: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));
    let post: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));

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

    app.add_systems(
        Startup,
        |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "list_teardown.xaml".to_string(),
                Arc::new(HOST_XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "list_teardown.xaml".to_string(),
                        size: UVec2::new(256, 256),
                        ..default()
                    },
                ))
                .id();
            let list = commands
                .spawn(UiList::new(view, "Inv").sorted_by(1, false))
                .id();
            for (label, weight) in [("A", 1), ("B", 2), ("C", 3)] {
                commands.spawn((
                    Row {
                        label: label.into(),
                        weight,
                    },
                    ListedIn(list),
                ));
            }
        },
    );

    let pre_sys = Arc::clone(&pre);
    let post_sys = Arc::clone(&post);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              diag: Res<NoesisDiagnostics>,
              views: Query<Entity, With<NoesisView>>,
              mut commands: Commands,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;
            if *frame == CAPTURE_PRE_AT {
                *pre_sys.lock().unwrap() = Some(diag.live_lists);
            }
            if *frame == DESPAWN_AT {
                // Despawn the view alone; teardown reaps its ListBinding and
                // despawn_orphan_lists takes the list entity with it.
                for e in &views {
                    commands.entity(e).despawn();
                }
            }
            if *frame == CAPTURE_POST_AT {
                *post_sys.lock().unwrap() = Some(diag.live_lists);
            }
            if *frame >= EXIT_AT {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let pre = pre
        .lock()
        .unwrap()
        .expect("pre-despawn live_lists captured");
    let post = post
        .lock()
        .unwrap()
        .expect("post-despawn live_lists captured");
    eprintln!("--- list teardown pre={pre} post={post} ---");

    // Before despawn: the binding is live (the UiList reconciled at least once).
    assert_eq!(
        pre, 1,
        "list binding should be live before despawn; got {pre} live lists",
    );
    // After despawn: the binding is reaped, no leak, drop order honored.
    assert_eq!(
        post, 0,
        "despawn must reap the view's list binding; {post} live lists still tracked",
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
