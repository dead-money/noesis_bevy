//! Integration test for the `NoesisInlines` bridge exercised through the headless harness.
//!
//! Drives a representative inline tree on one `TextBlock` covering every builder arm
//! (Run, Bold, Italic, Underline, Span, `LineBreak`, Hyperlink with `NavigateUri`), reads
//! the live structure back via `NoesisInlinesChanged`, and asserts four axes:
//! `count` (top-level inline count), `text` (depth-first Run concatenation),
//! `matched` (pointer identity of each built inline), `hyperlink_uris` (live `NavigateUri`).
//!
//! `Other` is the negative control: a `TextBlock` the bridge never touches; its count must stay 0.
//!
//! The bridge component starts empty and is populated after the scene is built so
//! change-detection fires on the first mutation rather than being lost before the view exists.
//!
//! Font-free: only inline structure and text DPs are read, no glyph rendering.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    InlineSpec, InlinesReadback, NoesisCamera, NoesisInlines, NoesisInlinesChanged, NoesisView,
    XamlRegistry,
};

mod common;
use common::{headless_app, run_until};

// Frame-gated stimulus: populate Body once the scene exists. Frames are instant
// under run_until; the exit predicate is the read-back, not this count.
const SET_AT_FRAME: usize = 10;

// Two empty TextBlocks: the bridge target and an un-touched negative control.
const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="320" Height="96">
  <TextBlock x:Name="Body"/>
  <TextBlock x:Name="Other"/>
</Grid>"##;

type Observed = Vec<(Entity, String, InlinesReadback)>;

// Covers every builder arm; seven top-level inlines.
fn inline_tree() -> Vec<InlineSpec> {
    vec![
        InlineSpec::run("Hello "),
        InlineSpec::bold([InlineSpec::run("World")]),
        InlineSpec::italic([InlineSpec::run("!")]),
        InlineSpec::span([
            InlineSpec::run(" ["),
            InlineSpec::bold([InlineSpec::run("nested")]),
        ]),
        InlineSpec::line_break(),
        InlineSpec::hyperlink("https://noesisengine.com/", [InlineSpec::run("link")]),
        InlineSpec::underline([InlineSpec::run(" end")]),
    ]
}

// Flattened text of `inline_tree()`: every Run's text, depth-first, no separators.
const EXPECTED_TEXT: &str = "Hello World! [nestedlink end";
const EXPECTED_TOP_LEVEL: usize = 7;

#[test]
fn inlines_bridge_builds_textblock_content() {
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "inlines.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "inlines.xaml".to_string(),
                        size: UVec2::new(320, 96),
                        ..default()
                    },
                    // Write-only bridge starts empty (no-op); filled after the
                    // scene exists so its one-shot apply isn't lost.
                    NoesisInlines::new(),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let observed_sys = Arc::clone(&observed);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut q: Query<&mut NoesisInlines>,
              mut changes: MessageReader<NoesisInlinesChanged>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for mut inlines in &mut q {
                    // Populate Body only; leave Other as the negative control.
                    // Watch both so the control's emptiness is observed too.
                    *inlines = NoesisInlines::new()
                        .set("Body", inline_tree())
                        .watching(["Body", "Other"]);
                }
            }

            for ev in changes.read() {
                observed_sys
                    .lock()
                    .unwrap()
                    .push((ev.view, ev.name.clone(), ev.value.clone()));
            }
        },
    );

    let latest = |got: &Observed, view: Entity, name: &str| -> Option<InlinesReadback> {
        got.iter()
            .rfind(|(e, n, _)| *e == view && n == name)
            .map(|(_, _, v)| v.clone())
    };

    // Exit once Body's built tree is live and the negative control has reported empty.
    let pred_observed = Arc::clone(&observed);
    let pred_view = Arc::clone(&view_entity);
    let converged = run_until(&mut app, 240, |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        let got = pred_observed.lock().unwrap();
        let body_ready = latest(&got, view, "Body")
            .is_some_and(|b| b.text == EXPECTED_TEXT && b.count == EXPECTED_TOP_LEVEL);
        let other_ready = latest(&got, view, "Other").is_some_and(|o| o.count == 0);
        body_ready && other_ready
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed NoesisInlinesChanged ---");
    for (e, name, value) in &got {
        eprintln!("  {e:?} {name} = {value:?}");
    }

    assert!(
        converged,
        "inlines never built Body and reported Other empty within 240 frames; \
         observed {got:?}",
    );

    let body = latest(&got, view, "Body").expect("Body read-back observed");

    assert_eq!(
        body.count, EXPECTED_TOP_LEVEL,
        "Body should have {EXPECTED_TOP_LEVEL} top-level inlines, got {}",
        body.count,
    );

    assert_eq!(
        body.text, EXPECTED_TEXT,
        "Body flattened inline text mismatch (proves runs + nested spans landed)",
    );

    assert!(
        body.matched,
        "every built top-level inline must be present by identity in the live TextBlock",
    );

    assert_eq!(
        body.hyperlink_uris,
        vec!["https://noesisengine.com/".to_string()],
        "the Hyperlink's NavigateUri should read back from the live object",
    );

    let other = latest(&got, view, "Other").expect("Other read-back observed");
    assert_eq!(
        other.count, 0,
        "negative control: un-bridged Other must have no inlines",
    );
    assert_eq!(other.text, "", "negative control: Other has no run text");
    assert!(
        other.hyperlink_uris.is_empty(),
        "negative control: Other has no hyperlinks",
    );
}
