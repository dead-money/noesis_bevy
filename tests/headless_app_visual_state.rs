//! Integration test for the `NoesisVisualState` bridge (`VisualStateManager::GoToState`),
//! run end-to-end through the real `NoesisPlugin` pipeline (headless, pipelined rendering on).
//!
//! The bridge has no read-back message, so its effect is observed via a `NoesisDp` watch on
//! `ActualWidth`. Driving "Widget" to "Big" must yield `ActualWidth = 50`; "Other" is left
//! undriven and must stay at `10` (negative control for wrong-entity routing regressions).
//!
//! The write-only component starts empty and is filled after the scene is built, because it
//! applies only on change-detection and mutating it before the view exists drops the apply.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    DpKind, DpValue, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisPlugin, NoesisView,
    NoesisVisualState, XamlRegistry,
};

const SET_AT_FRAME: usize = 10;
const EXIT_AT_FRAME: usize = 60;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="64">
  <Grid.Resources>
    <ControlTemplate x:Key="WidgetTemplate" TargetType="ContentControl">
      <Border x:Name="RootBorder" Width="10" Height="10" Background="#400000FF">
        <VisualStateManager.VisualStateGroups>
          <VisualStateGroup x:Name="SizeStates">
            <VisualState x:Name="Small"/>
            <VisualState x:Name="Big">
              <Storyboard>
                <DoubleAnimation Storyboard.TargetName="RootBorder"
                                 Storyboard.TargetProperty="Width"
                                 To="50" Duration="0:0:0"/>
              </Storyboard>
            </VisualState>
          </VisualStateGroup>
        </VisualStateManager.VisualStateGroups>
      </Border>
    </ControlTemplate>
  </Grid.Resources>

  <ContentControl x:Name="Widget"
                  HorizontalAlignment="Left" VerticalAlignment="Top"
                  Template="{StaticResource WidgetTemplate}"/>
  <ContentControl x:Name="Other"
                  HorizontalAlignment="Right" VerticalAlignment="Top"
                  Template="{StaticResource WidgetTemplate}"/>
</Grid>"##;

type Observed = Vec<(Entity, String, String, DpValue)>;

fn watcher() -> NoesisDp {
    NoesisDp::new()
        .watch("Widget", "ActualWidth", DpKind::F32) // driven to "Big"
        .watch("Other", "ActualWidth", DpKind::F32) // negative control
}

#[test]
fn visual_state_bridge_transitions_named_control() {
    noesis_license_from_env();

    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

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

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "states.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "states.xaml".to_string(),
                        size: UVec2::new(64, 64),
                        ..default()
                    },
                    // Starts empty; filled after the scene exists so the one-shot apply isn't dropped.
                    NoesisVisualState::new(),
                    watcher(),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let observed_sys = Arc::clone(&observed);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut q: Query<(&mut NoesisVisualState, &mut NoesisDp)>,
              mut changes: MessageReader<NoesisDpChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for (mut vs, _dp) in &mut q {
                    // Snap (no transition) Widget -> "Big"; leave Other alone.
                    *vs = NoesisVisualState::new().state("Widget", "Big", false);
                }
            }

            for ev in changes.read() {
                observed_sys.lock().unwrap().push((
                    ev.view,
                    ev.name.clone(),
                    ev.property.clone(),
                    ev.value.clone(),
                ));
            }

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed NoesisDpChanged ---");
    for (e, name, prop, value) in &got {
        eprintln!("  {e:?} {name}.{prop} = {value:?}");
    }

    let latest = |name: &str, prop: &str| -> Option<DpValue> {
        got.iter()
            .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
            .map(|(_, _, _, v)| v.clone())
    };

    assert_eq!(
        latest("Widget", "ActualWidth"),
        Some(DpValue::F32(50.0)),
        "visual-state: GoToState(\"Big\") should drive RootBorder.Width=50 \
         => Widget.ActualWidth 50 (default 10)",
    );
    // Negative control: the bridge must touch ONLY its target.
    assert_eq!(
        latest("Other", "ActualWidth"),
        Some(DpValue::F32(10.0)),
        "visual-state: an undriven control must stay in its default state \
         (ActualWidth 10)",
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
