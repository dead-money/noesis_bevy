//! Bevy-app-level integration test for the **write-only** `NoesisTypography`
//! bridge, exercised end-to-end through the real `NoesisPlugin` pipeline
//! (headless, pipelined rendering on).
//!
//! The bridge has five sub-features — `FontSize`, `FontFamily`, `FontWeight`,
//! `FontStyle`, `FontStretch` — each a distinct dispatch arm in
//! `apply_typography_for`. This test drives *all five* on one element and reads
//! every one back, so a regression in any single arm (panic, wrong property,
//! ref-count slip, dropped write) fails the test.
//!
//! Observation uses two independent read paths so the coverage is bluff-resistant:
//!
//!   * **`NoesisDp` watch** on `FontSize` (`f32`) — the generic, type-agnostic
//!     DP read. Proves the scalar write lands and (via the `Other` negative
//!     control) that the bridge touches only its target.
//!   * **`NoesisTypography` watch** on the typed `FontFamily` / `FontWeight` /
//!     `FontStyle` / `FontStretch` — the typography bridge's *own* read path.
//!     `FontWeight`/`Style`/`Stretch` are Noesis **enum** DPs and do not
//!     round-trip through `NoesisDp::get_i32` (same as `Visibility`); the typed
//!     getters are the only way to read them back. This is exclusive coverage of
//!     the typography glue, not shared with the generic DP bridge.
//!
//! Every assertion compares against the *authored* default in the XAML
//! (`FontSize=12`, `FontWeight=Normal`/400, `FontStyle=Normal`, `FontStretch=Normal`)
//! so each asserted value provably differs from the un-applied state. `Other` is
//! the negative control: left un-bridged, it must keep its authored defaults.
//!
//! The bridge component starts empty (no-op) and is filled in *after* the scene
//! is built, because it applies only on Bevy change-detection — mutating it before
//! the view exists would drop the one-shot apply.
//!
//! Font-free assertions (only DP values are read, no glyph rendering), so the
//! scene builds with no font gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    DpKind, DpValue, FontStretch, FontStyle, FontWeight, NoesisCamera, NoesisDp, NoesisDpChanged,
    NoesisPlugin, NoesisTypography, NoesisTypographyChanged, NoesisView, TypographyField,
    TypographyValue, XamlRegistry,
};

const SET_AT_FRAME: usize = 10;
const EXIT_AT_FRAME: usize = 60;

// Two TextBlocks with explicit, identical authored font properties — the
// un-applied defaults and the negative-control baseline. Every property the
// bridge can change is authored here so an assertion's "before" value is known.
const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="200" Height="64">
  <TextBlock x:Name="Title" FontSize="12" FontWeight="Normal" FontStyle="Normal" FontStretch="Normal" Text="Hello"/>
  <TextBlock x:Name="Other" FontSize="12" FontWeight="Normal" FontStyle="Normal" FontStretch="Normal" Text="World"/>
</Grid>"##;

type ObservedDp = Vec<(Entity, String, String, DpValue)>;
type ObservedTypo = Vec<(Entity, String, TypographyValue)>;

// Generic-DP watch: FontSize on the bridged Title and the un-bridged control.
fn dp_watcher() -> NoesisDp {
    NoesisDp::new()
        .watch("Title", "FontSize", DpKind::F32) // bridged target
        .watch("Other", "FontSize", DpKind::F32) // un-bridged negative control
}

// Typography-bridge watch: the typed (incl. enum) properties no DP i32 read can
// reach. Title is the bridged target; Other.FontWeight is the negative control.
fn typo_watcher() -> NoesisTypography {
    NoesisTypography::new()
        .watch("Title", TypographyField::FontFamily)
        .watch("Title", TypographyField::FontWeight)
        .watch("Title", TypographyField::FontStyle)
        .watch("Title", TypographyField::FontStretch)
        .watch("Other", TypographyField::FontWeight)
}

#[test]
fn typography_bridge_applies_all_font_properties() {
    noesis_license_from_env();

    let observed_dp: Arc<Mutex<ObservedDp>> = Arc::new(Mutex::new(Vec::new()));
    let observed_typo: Arc<Mutex<ObservedTypo>> = Arc::new(Mutex::new(Vec::new()));
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
                "typography.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "typography.xaml".to_string(),
                        size: UVec2::new(200, 64),
                        ..default()
                    },
                    // Write-only bridge starts empty (no-op); filled in after the
                    // scene exists so its one-shot apply isn't lost. The watch list
                    // also starts empty so no read fires before the apply.
                    NoesisTypography::new(),
                    NoesisDp::new(),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let observed_dp_sys = Arc::clone(&observed_dp);
    let observed_typo_sys = Arc::clone(&observed_typo);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut q: Query<(&mut NoesisTypography, &mut NoesisDp)>,
              mut dp_changes: MessageReader<NoesisDpChanged>,
              mut typo_changes: MessageReader<NoesisTypographyChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for (mut typo, mut dp) in &mut q {
                    // Restyle only Title across ALL five sub-features; leave Other
                    // as the negative control. Each authored default differs from
                    // the value set here, so a dropped/wrong-property write shows.
                    *typo = NoesisTypography::new()
                        .font_size("Title", 30.0)
                        .font_family("Title", "Arial")
                        .font_weight("Title", FontWeight::Bold)
                        .font_style("Title", FontStyle::Italic)
                        .font_stretch("Title", FontStretch::Condensed);
                    // Re-add the read-back watches (re-assign re-triggers change
                    // detection so the poll list is live again).
                    typo.watch = typo_watcher().watch;
                    *dp = dp_watcher();
                }
            }

            for ev in dp_changes.read() {
                observed_dp_sys.lock().unwrap().push((
                    ev.view,
                    ev.name.clone(),
                    ev.property.clone(),
                    ev.value.clone(),
                ));
            }
            for ev in typo_changes.read() {
                observed_typo_sys.lock().unwrap().push((
                    ev.view,
                    ev.name.clone(),
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
    let got_dp = observed_dp.lock().unwrap().clone();
    let got_typo = observed_typo.lock().unwrap().clone();
    eprintln!("--- observed NoesisDpChanged ---");
    for (e, name, prop, value) in &got_dp {
        eprintln!("  {e:?} {name}.{prop} = {value:?}");
    }
    eprintln!("--- observed NoesisTypographyChanged ---");
    for (e, name, value) in &got_typo {
        eprintln!("  {e:?} {name} = {value:?}");
    }

    // Latest scalar DP value seen for a watched (name, property) on our view.
    let latest_dp = |name: &str, prop: &str| -> Option<DpValue> {
        got_dp
            .iter()
            .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
            .map(|(_, _, _, v)| v.clone())
    };
    // Latest typed font value of the matching variant for a watched name.
    let latest_typo = |name: &str, want: fn(&TypographyValue) -> bool| -> Option<TypographyValue> {
        got_typo
            .iter()
            .rfind(|(e, n, v)| *e == view && n == name && want(v))
            .map(|(_, _, v)| v.clone())
    };

    // ── FontSize (generic DP read path) ──────────────────────────────────────
    assert_eq!(
        latest_dp("Title", "FontSize"),
        Some(DpValue::F32(30.0)),
        "FontSize: bridging Title.FontSize=30 should read back 30 (authored default 12)",
    );

    // ── FontFamily (typed read path) ─────────────────────────────────────────
    assert_eq!(
        latest_typo("Title", |v| matches!(v, TypographyValue::FontFamily(_))),
        Some(TypographyValue::FontFamily(Some("Arial".to_string()))),
        "FontFamily: bridging Title to \"Arial\" should read back its source",
    );

    // ── FontWeight (enum read path — not reachable via DP get_i32) ────────────
    assert_eq!(
        latest_typo("Title", |v| matches!(v, TypographyValue::FontWeight(_))),
        Some(TypographyValue::FontWeight(FontWeight::Bold)),
        "FontWeight: bridging Title to Bold(700) should read back Bold (authored Normal/400)",
    );

    // ── FontStyle (enum read path) ───────────────────────────────────────────
    assert_eq!(
        latest_typo("Title", |v| matches!(v, TypographyValue::FontStyle(_))),
        Some(TypographyValue::FontStyle(FontStyle::Italic)),
        "FontStyle: bridging Title to Italic should read back Italic (authored Normal)",
    );

    // ── FontStretch (enum read path) ─────────────────────────────────────────
    assert_eq!(
        latest_typo("Title", |v| matches!(v, TypographyValue::FontStretch(_))),
        Some(TypographyValue::FontStretch(FontStretch::Condensed)),
        "FontStretch: bridging Title to Condensed should read back Condensed (authored Normal)",
    );

    // ── Negative controls: the bridge must touch ONLY its target ─────────────
    // A "restyle everything" / wrong-entity-routing regression would change Other.
    assert_eq!(
        latest_dp("Other", "FontSize"),
        Some(DpValue::F32(12.0)),
        "negative control: un-bridged Other must keep authored FontSize 12",
    );
    assert_eq!(
        latest_typo("Other", |v| matches!(v, TypographyValue::FontWeight(_))),
        Some(TypographyValue::FontWeight(FontWeight::Normal)),
        "negative control: un-bridged Other must keep authored FontWeight Normal(400)",
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
