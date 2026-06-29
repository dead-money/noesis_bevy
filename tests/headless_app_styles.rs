//! Bevy-app-level integration test for the code-built `Style` bridge
//! ([`NoesisStyles`]), exercised end-to-end through the real `NoesisPlugin`
//! pipeline (headless, pipelined rendering on). Mirrors `headless_app_props.rs`.
//!
//! `NoesisStyles` is write-only (it pushes a built `Noesis::Style` into the live
//! view and emits no read-back of its own), so — exactly like the other
//! write-only bridges — the assertion observes the style's *actual effect*
//! through a [`NoesisDp`] watch and asserts the exact value. The element's
//! *default* value is the built-in negative control: a missing apply / wrong
//! target type / wrong-entity routing reads back the default and fails.
//!
//!   * **setter** → `Opacity` (`f32`): a `Style` targeting `Border` with a
//!     `Setter Opacity=0.5` drives `Styled.Opacity` to `0.5`, not the default
//!     `1.0`.
//!   * **setter** → `Width` (`f32`): the same style's `Setter Width=40` drives
//!     `Styled.Width` to `40`. The element authors no local `Width`, so the
//!     style is the only value source; a no-op apply leaves it `NaN`/unset.
//!   * **negative control** → `Plain.Opacity` (`f32`): an unstyled sibling stays
//!     at the default `1.0`. A "style everything" / wrong-entity regression
//!     would pull it to `0.5` too.
//!
//! The component starts empty (no-op) and is filled in *after* the scene is
//! built, because `set_style` applies only on Bevy change-detection — mutating
//! it before the view exists would drop the one-shot apply (and a style is
//! sealed on first apply).
//!
//! Font-free XAML (only DP values are asserted, no glyph rendering), so the scene
//! builds with no font gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::{
    DpKind, DpValue, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisPlugin, NoesisStyles,
    NoesisView, StyleSpec, XamlRegistry,
};

const SET_AT_FRAME: usize = 10;
const EXIT_AT_FRAME: usize = 60;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <Border x:Name="Styled" Background="#400000FF"/>
  <Border x:Name="Plain" Background="#4000FF00"/>
</Grid>"##;

type Observed = Vec<(Entity, String, String, DpValue)>;

fn watcher() -> NoesisDp {
    NoesisDp::new()
        .watch("Styled", "Opacity", DpKind::F32) // setter effect
        .watch("Styled", "Width", DpKind::F32) // setter effect (no local value)
        .watch("Plain", "Opacity", DpKind::F32) // negative control
}

#[test]
fn code_built_style_applies_to_named_element() {
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
                "styles.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "styles.xaml".to_string(),
                        size: UVec2::new(64, 32),
                        ..default()
                    },
                    // Starts empty (no-op); filled in after the scene exists so
                    // the one-shot style apply isn't lost.
                    NoesisStyles::new(),
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
              mut q: Query<(&mut NoesisStyles, &mut NoesisDp)>,
              mut changes: MessageReader<NoesisDpChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for (mut styles, _dp) in &mut q {
                    *styles = NoesisStyles::new().apply(
                        "Styled",
                        StyleSpec::new("Border")
                            .setter("Opacity", DpValue::F32(0.5))
                            .setter("Width", DpValue::F32(40.0)),
                    );
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
        latest("Styled", "Opacity"),
        Some(DpValue::F32(0.5)),
        "setter: a Style with Setter Opacity=0.5 should drive Styled.Opacity to 0.5 (default 1.0)",
    );
    assert_eq!(
        latest("Styled", "Width"),
        Some(DpValue::F32(40.0)),
        "setter: a Style with Setter Width=40 should drive Styled.Width to 40 (no local value)",
    );
    // Negative control: the style targets one element only — a "style everything"
    // or wrong-entity-routing regression would flip Plain too.
    assert_eq!(
        latest("Plain", "Opacity"),
        Some(DpValue::F32(1.0)),
        "negative control: an unstyled sibling must stay at the default Opacity 1.0",
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
