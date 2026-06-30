//! Primitive 3 (events = observers), per-row half: clicking a templated list row
//! surfaces as a [`UiClicked`] `EntityEvent` whose target IS that row's entity —
//! recovered with no `x:Name` by walking the clicked element's `DataContext` to
//! the row's hidden `__entity` field.
//!
//! Drives a real headless [`NoesisPlugin`] app: an `ItemsControl` (`x:Name="Inv"`)
//! bound by [`UiList`] to three rows spawned as entities with [`ListedIn`]. After
//! the rows realize, a left mouse down-then-up is injected over the top row, and a
//! global observer must receive a `UiClicked` targeting that row's entity.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::input::{NoesisInputEvent, NoesisInputQueue};
use noesis_bevy::routed_events::MouseButton;
use noesis_bevy::{
    ListedIn, NoesisCamera, NoesisListAppExt, NoesisPlugin, NoesisView, NoesisViewModel, UiClicked,
    UiList, XamlRegistry,
};

// A non-virtualizing ItemsControl (default StackPanel items panel) whose rows are
// fixed-height, full-width, hit-testable Borders, so each row realizes headless
// and sits at a known position. Rows order A(1), B(2), C(3) top-to-bottom; row A
// spans y=[0,40]. A plain ItemsControl has no Selector handlers to swallow the
// MouseLeftButtonUp before it bubbles to the control.
const HOST_XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="256" Height="256">
  <ItemsControl x:Name="Inv">
    <ItemsControl.ItemTemplate>
      <DataTemplate>
        <Border Background="#FF404040" Height="40" Width="256">
          <TextBlock Text="{Binding label}"/>
        </Border>
      </DataTemplate>
    </ItemsControl.ItemTemplate>
  </ItemsControl>
</Grid>"##;

/// A list row: `label` (string) + `weight` (i32 sort key at index 1).
#[derive(Component, NoesisViewModel)]
struct Row {
    label: String,
    weight: i32,
}

const PRESS_AT: usize = 30;
const RELEASE_AT: usize = 32;
const EXIT_AT: usize = 64;

/// What an observer captured off a `UiClicked`: (target entity, view).
type Observed = Vec<(Entity, Entity)>;

#[test]
fn list_row_click_triggers_uiclicked_targeting_the_row() {
    noesis_license_from_env();

    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let entities: Arc<Mutex<Option<(Entity, Entity, Entity, Entity)>>> = Arc::new(Mutex::new(None));

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

    // Global observer: the push-based half of Primitive 3, per row.
    let observed_obs = Arc::clone(&observed);
    app.add_observer(move |on: On<UiClicked>| {
        observed_obs
            .lock()
            .unwrap()
            .push((on.event_target(), on.view));
    });

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
                    // Order rows by weight ascending: A(1), B(2), C(3).
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
            *entities_startup.lock().unwrap() = Some((view, a, b, c));
        },
    );

    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut input: ResMut<NoesisInputQueue>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;
            // Click the top row (A), centered at y=20 of its 40px container.
            if *frame == PRESS_AT {
                input.push(NoesisInputEvent::MouseMove { x: 100, y: 20 });
                input.push(NoesisInputEvent::MouseButton {
                    down: true,
                    x: 100,
                    y: 20,
                    button: MouseButton::Left,
                });
            }
            if *frame == RELEASE_AT {
                input.push(NoesisInputEvent::MouseButton {
                    down: false,
                    x: 100,
                    y: 20,
                    button: MouseButton::Left,
                });
            }
            if *frame >= EXIT_AT {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let (view, a, _b, _c) = entities.lock().unwrap().expect("rows spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed UiClicked (row a={a:?}) ---");
    for (target, v) in &got {
        eprintln!("  target={target:?} view={v:?}");
    }

    let hit = got
        .iter()
        .find(|(target, _)| *target == a)
        .expect("observer should have received a UiClicked targeting the clicked row entity (A)");
    assert_eq!(
        hit.1, view,
        "per-row UiClicked.view should be the owning view entity",
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
