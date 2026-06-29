//! Bevy-app-level integration test for the **write-only** `NoesisVisualState`
//! bridge (`VisualStateManager::GoToState`), exercised end-to-end through the
//! real `NoesisPlugin` pipeline (headless, pipelined rendering on).
//!
//! The bridge has no read-back message of its own, so — like the other
//! write-only element bridges — we observe its *actual effect* through a
//! `NoesisDp` watch on a scalar dependency property the transition provably
//! changes, and assert the exact value:
//!
//!   * Two `ContentControl`s ("Widget", "Other") share a `ControlTemplate`
//!     whose root `Border` (`RootBorder`, Width=10) carries a `SizeStates`
//!     `VisualStateGroup`. The "Big" state runs a zero-duration animation that
//!     drives `RootBorder.Width` to 50; each control is `Left`/`Top`-aligned
//!     and sized to its template, so its `ActualWidth` tracks `RootBorder`.
//!   * Driving **only** "Widget" to "Big" ⇒ `Widget.ActualWidth = 50`, not the
//!     default `10`. The default `10` is the built-in negative control: a
//!     missing apply / wrong-entity routing / inverted change-detection reads
//!     back `10` and fails.
//!   * "Other" is left undriven ⇒ stays `ActualWidth = 10`. A "go everything to
//!     Big" or wrong-name-resolution regression would grow it to `50`.
//!
//! The write-only component starts empty (no-op) and is filled in *after* the
//! scene is built, because it applies only on Bevy change-detection — mutating
//! it before the view exists would drop the one-shot apply.
//!
//! Font-free XAML (only DP values are asserted, no glyph rendering), so the
//! scene builds with no font gate.

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
                    // Write-only component starts empty (no-op); filled in after
                    // the scene exists so its one-shot apply isn't lost.
                    NoesisVisualState::new(),
                    // The DP watcher polls every frame regardless of changes.
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

    // Latest value seen for a watched (name, property) on our view.
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
