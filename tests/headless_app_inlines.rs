//! Bevy-app-level integration test for the **formatted-text** `NoesisInlines`
//! bridge, exercised end-to-end through the real `NoesisPlugin` pipeline
//! (headless, pipelined rendering on).
//!
//! The bridge builds a `TextBlock`'s rich `Inlines` from a declarative
//! [`InlineSpec`] tree. This test drives a representative tree on one `TextBlock`
//! that touches every builder arm — `Run`, `Bold`, `Italic`, `Underline`, `Span`,
//! `LineBreak`, and `Hyperlink` (with a `NavigateUri`) — including a nested span,
//! then reads the resulting **live** structure back via `NoesisInlinesChanged`.
//!
//! The read-back is bluff-resistant on three independent axes, all re-read from
//! live Noesis objects (not echoed from the Rust spec):
//!
//!   * **`count`** — the number of *top-level* inlines actually in
//!     `TextBlock.Inlines`. A no-op / dropped apply leaves this at 0.
//!   * **`text`** — the depth-first concatenation of every live `Run`'s text.
//!     This catches wrong/empty runs and proves nesting (the bold/italic/span
//!     children's text only appears if their sub-collections were populated).
//!   * **`matched`** — every top-level inline the bridge built is present, *by
//!     pointer identity*, at its expected index in the live collection. This is
//!     the bluff-killer: it proves the exact objects we created are this
//!     `TextBlock`'s content, not some look-alike.
//!   * **`hyperlink_uris`** — the live `NavigateUri` of the `Hyperlink`, proving
//!     the URI write landed.
//!
//! `Other` is the negative control: an authored `TextBlock` the bridge never
//! touches. Its read-back must stay empty (`count == 0`), proving the bridge
//! writes only its target.
//!
//! The bridge component starts empty (no-op) and is filled in *after* the scene
//! is built, because it applies only on Bevy change-detection — mutating it
//! before the view exists would drop the one-shot apply.
//!
//! Font-free: only inline structure / text DPs are read, no glyph rendering, so
//! the scene builds with no font gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::{
    InlineSpec, InlinesReadback, NoesisCamera, NoesisInlines, NoesisInlinesChanged, NoesisPlugin,
    NoesisView, XamlRegistry,
};

const SET_AT_FRAME: usize = 10;
const EXIT_AT_FRAME: usize = 60;

// Two empty TextBlocks: the bridge target and an un-touched negative control.
const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="320" Height="96">
  <TextBlock x:Name="Body"/>
  <TextBlock x:Name="Other"/>
</Grid>"##;

type Observed = Vec<(Entity, String, InlinesReadback)>;

// The inline tree under test: Run, Bold(Run), Italic(Run), nested Span(Run, Bold(Run)),
// LineBreak, Hyperlink(uri, Run), Underline(Run). Seven top-level inlines.
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
              mut changes: MessageReader<NoesisInlinesChanged>,
              mut exit: MessageWriter<AppExit>| {
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

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed NoesisInlinesChanged ---");
    for (e, name, value) in &got {
        eprintln!("  {e:?} {name} = {value:?}");
    }

    // Latest read-back for a watched name on our view.
    let latest = |name: &str| -> Option<InlinesReadback> {
        got.iter()
            .rfind(|(e, n, _)| *e == view && n == name)
            .map(|(_, _, v)| v.clone())
    };

    let body = latest("Body").expect("Body read-back observed");

    // ── count: the live top-level inline count ───────────────────────────────
    assert_eq!(
        body.count, EXPECTED_TOP_LEVEL,
        "Body should have {EXPECTED_TOP_LEVEL} top-level inlines, got {}",
        body.count,
    );

    // ── text: flattened live Run text across all nesting levels ──────────────
    assert_eq!(
        body.text, EXPECTED_TEXT,
        "Body flattened inline text mismatch (proves runs + nested spans landed)",
    );

    // ── matched: pointer identity of every top-level inline (bluff-killer) ───
    assert!(
        body.matched,
        "every built top-level inline must be present by identity in the live TextBlock",
    );

    // ── hyperlink_uris: the live NavigateUri ─────────────────────────────────
    assert_eq!(
        body.hyperlink_uris,
        vec!["https://noesisengine.com/".to_string()],
        "the Hyperlink's NavigateUri should read back from the live object",
    );

    // ── negative control: the un-bridged TextBlock stays empty ───────────────
    let other = latest("Other").expect("Other read-back observed");
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

fn noesis_license_from_env() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
}
