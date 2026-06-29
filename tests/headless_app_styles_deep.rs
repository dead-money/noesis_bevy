//! Bevy-app-level integration test for the *deep* styling slice of the
//! code-built `Style` bridge ([`NoesisStyles`]): `BasedOn` inheritance chains,
//! [`DataTriggerSpec`] (binding-value driven), and [`MultiTriggerSpec`]
//! (all-conditions-hold). Exercised end-to-end through the real `NoesisPlugin`
//! pipeline (headless, pipelined rendering on). Mirrors `headless_app_styles.rs`.
//!
//! `NoesisStyles` is write-only, so — like the other write-only bridges — every
//! assertion observes the style's *actual effect* through a [`NoesisDp`] watch and
//! asserts the exact value, with the element's *default* as the negative control:
//!
//!   * **`BasedOn` chain** → `Chain.Opacity` / `Height` / `Width`: a 3-level
//!     `Border` style (own `Opacity=0.5`, `BasedOn` a middle style `Height=12`,
//!     `BasedOn` a root style `Width=40`) drives all three values onto the
//!     element. `Width=40` reaches it only through two `BasedOn` hops, so reading
//!     `40` proves the chain links; a broken `set_based_on` leaves `Width` unset.
//!   * **`DataTrigger`** → `Trig.Opacity` (`f32`): a style whose `DataTrigger`
//!     binds `RelativeSource Self` `Path=Tag` with `Value="active"` and a
//!     `Setter Opacity=0.5` fires because the element authors `Tag="active"`,
//!     pulling `Opacity` to `0.5`.
//!   * **`DataTrigger` negative control** → `TrigOff.Opacity`: the *same* style
//!     on an element with `Tag="idle"` does **not** fire, so it stays at `1.0`.
//!   * **`MultiTrigger`** → `Multi.Opacity` (`f32`): a `MultiTrigger` whose two
//!     conditions (`IsEnabled=true` and `IsHitTestVisible=true`, both default
//!     true) hold drives `Opacity` to `0.5`.
//!   * **`MultiTrigger` negative control** → `MultiOff.Opacity`: the same style on
//!     an element authored `IsEnabled="False"` breaks one condition, so it stays
//!     at `1.0`.
//!   * **overall negative control** → `Plain.Opacity`: an unstyled sibling stays
//!     at the default `1.0`.
//!
//! Components start empty (no-op) and are filled in *after* the scene is built,
//! because `set_style` applies only on Bevy change-detection (and a style is
//! sealed on first apply). Font-free XAML (only DP values are asserted).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    DataTriggerSpec, DpKind, DpValue, MultiTriggerSpec, NoesisCamera, NoesisDp, NoesisDpChanged,
    NoesisPlugin, NoesisStyles, NoesisView, StyleSpec, XamlRegistry,
};

const SET_AT_FRAME: usize = 10;
const EXIT_AT_FRAME: usize = 60;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <Border x:Name="Chain" Background="#400000FF"/>
  <Border x:Name="Trig" Tag="active" Background="#4000FF00"/>
  <Border x:Name="TrigOff" Tag="idle" Background="#40FF0000"/>
  <Border x:Name="Multi" Background="#4000FFFF"/>
  <Border x:Name="MultiOff" IsEnabled="False" Background="#40FF00FF"/>
  <Border x:Name="Plain" Background="#40FFFF00"/>
</Grid>"##;

type Observed = Vec<(Entity, String, String, DpValue)>;

fn watcher() -> NoesisDp {
    NoesisDp::new()
        .watch("Chain", "Opacity", DpKind::F32) // BasedOn: own setter
        .watch("Chain", "Height", DpKind::F32) // BasedOn: 1 hop
        .watch("Chain", "Width", DpKind::F32) // BasedOn: 2 hops
        .watch("Trig", "Opacity", DpKind::F32) // DataTrigger fires
        .watch("TrigOff", "Opacity", DpKind::F32) // DataTrigger negative control
        .watch("Multi", "Opacity", DpKind::F32) // MultiTrigger fires
        .watch("MultiOff", "Opacity", DpKind::F32) // MultiTrigger negative control
        .watch("Plain", "Opacity", DpKind::F32) // overall negative control
}

#[test]
fn deep_styles_apply_basedon_chain_and_triggers() {
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
                "styles_deep.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "styles_deep.xaml".to_string(),
                        size: UVec2::new(64, 32),
                        ..default()
                    },
                    NoesisStyles::new(),
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
              mut q: Query<&mut NoesisStyles>,
              mut changes: MessageReader<NoesisDpChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                // A 3-level BasedOn chain: own Opacity, +Height one hop up,
                // +Width two hops up.
                let chain = StyleSpec::new("Border")
                    .setter("Opacity", DpValue::F32(0.5))
                    .based_on(
                        StyleSpec::new("Border")
                            .setter("Height", DpValue::F32(12.0))
                            .based_on(StyleSpec::new("Border").setter("Width", DpValue::F32(40.0))),
                    );
                // A DataTrigger keyed off the element's own Tag.
                let trig = StyleSpec::new("Border").data_trigger(
                    DataTriggerSpec::new("Tag", DpValue::Str("active".into()))
                        .relative_source_self()
                        .setter("Opacity", DpValue::F32(0.5)),
                );
                // A MultiTrigger: two default-true conditions both hold.
                let multi = StyleSpec::new("Border").multi_trigger(
                    MultiTriggerSpec::new()
                        .condition("IsEnabled", DpValue::Bool(true))
                        .condition("IsHitTestVisible", DpValue::Bool(true))
                        .setter("Opacity", DpValue::F32(0.5)),
                );

                for mut styles in &mut q {
                    *styles = NoesisStyles::new()
                        .apply("Chain", chain.clone())
                        .apply("Trig", trig.clone())
                        .apply("TrigOff", trig.clone())
                        .apply("Multi", multi.clone())
                        .apply("MultiOff", multi.clone());
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

    // BasedOn chain: the derived style's own setter, plus one and two hops up.
    assert_eq!(
        latest("Chain", "Opacity"),
        Some(DpValue::F32(0.5)),
        "BasedOn: the derived style's own Setter Opacity=0.5 (default 1.0)",
    );
    assert_eq!(
        latest("Chain", "Height"),
        Some(DpValue::F32(12.0)),
        "BasedOn: Height=12 inherited one hop up the chain (no local value)",
    );
    assert_eq!(
        latest("Chain", "Width"),
        Some(DpValue::F32(40.0)),
        "BasedOn: Width=40 inherited two hops up the chain — proves the chain links",
    );

    // DataTrigger: fires for Tag="active", not for Tag="idle".
    assert_eq!(
        latest("Trig", "Opacity"),
        Some(DpValue::F32(0.5)),
        "DataTrigger: Tag=active matches Value, so Setter Opacity=0.5 applies (default 1.0)",
    );
    assert_eq!(
        latest("TrigOff", "Opacity"),
        Some(DpValue::F32(1.0)),
        "DataTrigger negative control: Tag=idle does not match, so Opacity stays 1.0",
    );

    // MultiTrigger: fires when both conditions hold, not when one is broken.
    assert_eq!(
        latest("Multi", "Opacity"),
        Some(DpValue::F32(0.5)),
        "MultiTrigger: both default-true conditions hold, so Setter Opacity=0.5 applies",
    );
    assert_eq!(
        latest("MultiOff", "Opacity"),
        Some(DpValue::F32(1.0)),
        "MultiTrigger negative control: IsEnabled=False breaks a condition, so Opacity stays 1.0",
    );

    // Overall negative control.
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
