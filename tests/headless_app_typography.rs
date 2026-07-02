//! Headless integration test for `NoesisTypography`, exercising all five properties
//! (`FontSize`, `FontFamily`, `FontWeight`, `FontStyle`, `FontStretch`) against both read paths.
//!
//! FontWeight/Style/Stretch are Noesis enum DPs that do not round-trip through
//! `NoesisDp::get_i32`; the typed `NoesisTypography` getters are the only read path.
//!
//! `NoesisTypography` spawns empty and is populated after the scene builds because
//! Bevy change-detection drops a write made before the view exists.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    DpKind, DpValue, FontStretch, FontStyle, FontWeight, NoesisCamera, NoesisDp, NoesisDpChanged,
    NoesisTypography, NoesisTypographyChanged, NoesisView, TypographyField, TypographyValue,
    XamlRegistry,
};

mod common;
use common::{headless_app, run_until};

const SET_AT_FRAME: usize = 10;

// Both elements authored with identical defaults so every assertion has a known
// "before" value. Other is left un-bridged as the negative control.
const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="200" Height="64">
  <TextBlock x:Name="Title" FontSize="12" FontWeight="Normal" FontStyle="Normal" FontStretch="Normal" Text="Hello"/>
  <TextBlock x:Name="Other" FontSize="12" FontWeight="Normal" FontStyle="Normal" FontStretch="Normal" Text="World"/>
</Grid>"##;

type ObservedDp = Vec<(Entity, String, String, DpValue)>;
type ObservedTypo = Vec<(Entity, String, TypographyValue)>;

fn dp_watcher() -> NoesisDp {
    NoesisDp::new()
        .watch("Title", "FontSize", DpKind::F32)
        .watch("Other", "FontSize", DpKind::F32)
}

// Enum DPs have no i32 read path; typed watch required for FontWeight/Style/Stretch.
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
    let observed_dp: Arc<Mutex<ObservedDp>> = Arc::new(Mutex::new(Vec::new()));
    let observed_typo: Arc<Mutex<ObservedTypo>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

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
                    // Empty on spawn; populated after scene build so the one-shot apply isn't lost.
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
              mut typo_changes: MessageReader<NoesisTypographyChanged>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for (mut typo, mut dp) in &mut q {
                    // Restyle only Title; leave Other as the negative control.
                    *typo = NoesisTypography::new()
                        .font_size("Title", 30.0)
                        .font_family("Title", "Arial")
                        .font_weight("Title", FontWeight::Bold)
                        .font_style("Title", FontStyle::Italic)
                        .font_stretch("Title", FontStretch::Condensed);
                    // Re-assign triggers change detection, re-activating the poll list.
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
        },
    );

    // Exit once every restyled Title property has read back (FontSize via DP; the
    // three enum props + FontFamily via the typed watch). Negative controls are
    // asserted after the run.
    let pred_dp = Arc::clone(&observed_dp);
    let pred_typo = Arc::clone(&observed_typo);
    let pred_view = Arc::clone(&view_entity);
    let converged = run_until(&mut app, 240, move |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        let dp = pred_dp.lock().unwrap();
        let typo = pred_typo.lock().unwrap();
        let dp_is = |name: &str, prop: &str, want: &DpValue| {
            dp.iter()
                .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
                .is_some_and(|(_, _, _, v)| v == want)
        };
        let typo_has = |name: &str, want: &TypographyValue| {
            typo.iter()
                .any(|(e, n, v)| *e == view && n == name && v == want)
        };
        dp_is("Title", "FontSize", &DpValue::F32(30.0))
            && typo_has(
                "Title",
                &TypographyValue::FontFamily(Some("Arial".to_string())),
            )
            && typo_has("Title", &TypographyValue::FontWeight(FontWeight::Bold))
            && typo_has("Title", &TypographyValue::FontStyle(FontStyle::Italic))
            && typo_has(
                "Title",
                &TypographyValue::FontStretch(FontStretch::Condensed),
            )
    });

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

    let latest_dp = |name: &str, prop: &str| -> Option<DpValue> {
        got_dp
            .iter()
            .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
            .map(|(_, _, _, v)| v.clone())
    };
    let latest_typo = |name: &str, want: fn(&TypographyValue) -> bool| -> Option<TypographyValue> {
        got_typo
            .iter()
            .rfind(|(e, n, v)| *e == view && n == name && want(v))
            .map(|(_, _, v)| v.clone())
    };

    assert!(
        converged,
        "restyled Title font properties never all read back within 240 frames; \
         dp={got_dp:?} typo={got_typo:?}",
    );

    assert_eq!(
        latest_dp("Title", "FontSize"),
        Some(DpValue::F32(30.0)),
        "FontSize: bridging Title.FontSize=30 should read back 30 (authored default 12)",
    );

    assert_eq!(
        latest_typo("Title", |v| matches!(v, TypographyValue::FontFamily(_))),
        Some(TypographyValue::FontFamily(Some("Arial".to_string()))),
        "FontFamily: bridging Title to \"Arial\" should read back its source",
    );

    assert_eq!(
        latest_typo("Title", |v| matches!(v, TypographyValue::FontWeight(_))),
        Some(TypographyValue::FontWeight(FontWeight::Bold)),
        "FontWeight: bridging Title to Bold(700) should read back Bold (authored Normal/400)",
    );

    assert_eq!(
        latest_typo("Title", |v| matches!(v, TypographyValue::FontStyle(_))),
        Some(TypographyValue::FontStyle(FontStyle::Italic)),
        "FontStyle: bridging Title to Italic should read back Italic (authored Normal)",
    );

    assert_eq!(
        latest_typo("Title", |v| matches!(v, TypographyValue::FontStretch(_))),
        Some(TypographyValue::FontStretch(FontStretch::Condensed)),
        "FontStretch: bridging Title to Condensed should read back Condensed (authored Normal)",
    );

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
