//! Component-removal reap regression (audit P0.9): removing a bridge component
//! from a *live* view must reap that view's render-side state, symmetrically with
//! the entity-despawn teardown — not leak it for the life of the process.
//!
//! Before the per-bridge `RemovedComponents<C>` reap systems, reconcile only
//! visited entities that still *had* the component, so dropping (say) a `UiList`
//! off a view that stayed alive left its `ListBinding` — collection, realized row
//! instances, and row-class registration — bound and rendering forever.
//!
//! This drives a view with a `ListBox` + entity-rows until its binding is live
//! (`live_lists == 1`), then `remove::<UiList>()` while keeping the view (and its
//! scene) alive, and asserts the binding drains to 0 with the scene still live —
//! i.e. the reap ran off component removal, and did *not* tear the scene down.
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
const REMOVE_AT: usize = 21;
const CAPTURE_POST_AT: usize = 45;
const EXIT_AT: usize = 55;

#[test]
fn removing_uilist_from_a_live_view_reaps_its_binding() {
    noesis_license_from_env();

    let pre: Arc<Mutex<Option<(usize, usize)>>> = Arc::new(Mutex::new(None));
    let post: Arc<Mutex<Option<(usize, usize)>>> = Arc::new(Mutex::new(None));

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
                "removal_reap.xaml".to_string(),
                Arc::new(HOST_XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "removal_reap.xaml".to_string(),
                        size: UVec2::new(256, 256),
                        ..default()
                    },
                    UiList::new("Inv").sorted_by(1, false),
                ))
                .id();
            for (label, weight) in [("A", 1), ("B", 2), ("C", 3)] {
                commands.spawn((
                    Row {
                        label: label.into(),
                        weight,
                    },
                    ListedIn(view),
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
              views: Query<Entity, With<UiList>>,
              mut commands: Commands,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;
            if *frame == CAPTURE_PRE_AT {
                *pre_sys.lock().unwrap() = Some((diag.live_lists, diag.live_scenes));
            }
            if *frame == REMOVE_AT {
                // Drop only the UiList; the view (and its scene) stay live. The
                // reap must run off RemovedComponents<UiList>, not despawn.
                for e in &views {
                    commands.entity(e).remove::<UiList>();
                }
            }
            if *frame == CAPTURE_POST_AT {
                *post_sys.lock().unwrap() = Some((diag.live_lists, diag.live_scenes));
            }
            if *frame >= EXIT_AT {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let (pre_lists, pre_scenes) = pre.lock().unwrap().expect("pre-removal snapshot captured");
    let (post_lists, post_scenes) = post
        .lock()
        .unwrap()
        .expect("post-removal snapshot captured");
    eprintln!(
        "--- removal reap pre=(lists={pre_lists}, scenes={pre_scenes}) \
         post=(lists={post_lists}, scenes={post_scenes}) ---"
    );

    // Before removal: the binding and the scene are both live.
    assert_eq!(pre_lists, 1, "list binding should be live before removal");
    assert_eq!(pre_scenes, 1, "view scene should be live before removal");

    // After removal: the binding is reaped, but the view's scene stays live —
    // this is the component-removal path, not the despawn path.
    assert_eq!(
        post_lists, 0,
        "removing UiList must reap the view's list binding; {post_lists} still tracked",
    );
    assert_eq!(
        post_scenes, 1,
        "removing UiList must NOT tear down the view's scene; {post_scenes} live scenes",
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
