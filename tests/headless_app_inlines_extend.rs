//! Integration test for the extended `NoesisInlines` bridge: re-apply,
//! `TextDecorations`, and `InlineUIContainer`, verified by reading back from
//! live Noesis objects (not echoing the spec), so a no-op apply, stale
//! collection, or dropped write fails.
//!
//! Font-free: only inline structure, decorations, and hosted-child identity
//! are asserted.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    InlineSpec, InlinesReadback, NoesisCamera, NoesisInlines, NoesisInlinesChanged, NoesisView,
    TextDecorations, XamlRegistry,
};

mod common;
use common::{headless_app, run_until};

// Frame-gated stimulus: apply the initial tree once the scene exists, then
// re-apply the replacement a few frames later. Frames are instant under
// run_until; the exit predicate below is the terminal read-back, not a count.
const APPLY_AT_FRAME: usize = 10;
const REAPPLY_AT_FRAME: usize = 30;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="320" Height="96">
  <TextBlock x:Name="Body"/>
</Grid>"##;

// Rectangle, not a glyph element: avoids a font dependency.
const CHILD_XAML: &str = r#"<Rectangle xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Width="10" Height="10" Fill="Red"/>"#;

fn initial_tree() -> Vec<InlineSpec> {
    vec![
        InlineSpec::run("first"),
        InlineSpec::decorated(TextDecorations::Strikethrough, [InlineSpec::run("X")]),
    ]
}

fn replacement_tree() -> Vec<InlineSpec> {
    vec![
        InlineSpec::run("second "),
        InlineSpec::ui_container(CHILD_XAML),
        InlineSpec::decorated(TextDecorations::OverLine, [InlineSpec::run("Y")]),
    ]
}

type Observed = Vec<(Entity, String, InlinesReadback)>;

#[test]
fn inlines_bridge_reapply_decorations_and_ui_container() {
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "inlines_extend.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "inlines_extend.xaml".to_string(),
                        size: UVec2::new(320, 96),
                        ..default()
                    },
                    // filled later; avoids losing the first apply to change-detection timing
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

            if *frame == APPLY_AT_FRAME {
                for mut inlines in &mut q {
                    *inlines = NoesisInlines::new()
                        .set("Body", initial_tree())
                        .watching(["Body"]);
                }
            }
            if *frame == REAPPLY_AT_FRAME {
                for mut inlines in &mut q {
                    *inlines = NoesisInlines::new()
                        .set("Body", replacement_tree())
                        .watching(["Body"]);
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

    // Exit once both the initial tree ("firstX") and the re-applied replacement
    // ("second Y") have been read back from the live TextBlock.
    let pred_observed = Arc::clone(&observed);
    let pred_view = Arc::clone(&view_entity);
    let converged = run_until(&mut app, 240, |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        let got = pred_observed.lock().unwrap();
        let saw_initial = got
            .iter()
            .any(|(e, n, r)| *e == view && n == "Body" && r.text == "firstX");
        let latest_is_replacement = got
            .iter()
            .rfind(|(e, n, _)| *e == view && n == "Body")
            .is_some_and(|(_, _, r)| r.text == "second Y" && r.hosted_ui == 1);
        saw_initial && latest_is_replacement
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed NoesisInlinesChanged ---");
    for (e, name, value) in &got {
        eprintln!("  {e:?} {name} = {value:?}");
    }

    assert!(
        converged,
        "inlines re-apply never converged (initial 'firstX' then replacement \
         'second Y') within 240 frames; observed {got:?}",
    );

    let body_reads: Vec<&InlinesReadback> = got
        .iter()
        .filter(|(e, n, _)| *e == view && n == "Body")
        .map(|(_, _, v)| v)
        .collect();

    let initial = body_reads
        .iter()
        .find(|r| r.text == "firstX")
        .expect("initial inline content (text 'firstX') should be observed");
    assert_eq!(initial.count, 2, "initial tree has 2 top-level inlines");
    assert!(initial.matched, "initial built inlines present by identity");
    assert_eq!(
        initial.decorations,
        vec![TextDecorations::Strikethrough],
        "initial decorated Span reads Strikethrough from the live object",
    );
    assert_eq!(initial.hosted_ui, 0, "initial tree hosts no UIElement");

    let latest = body_reads
        .last()
        .expect("at least one Body read-back observed");
    assert_eq!(
        latest.text, "second Y",
        "re-apply: live text must be the replacement tree's, not the initial's",
    );
    assert_eq!(
        latest.count, 3,
        "re-apply: replacement has 3 top-level inlines (old 2 were cleared)",
    );
    assert!(
        latest.matched,
        "re-apply: the replacement's built inlines are the live content by identity",
    );
    assert_eq!(
        latest.decorations,
        vec![TextDecorations::OverLine],
        "re-apply: only the replacement's OverLine span remains (Strikethrough gone)",
    );
    assert_eq!(
        latest.hosted_ui, 1,
        "InlineUIContainer hosts the parsed Button by pointer identity",
    );

    // confirms clear-and-rebuild happened, not append or no-op
    assert_ne!(
        latest.text, "firstX",
        "re-apply must replace the initial content, not retain it",
    );
}
