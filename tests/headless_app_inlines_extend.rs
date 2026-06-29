//! Integration test for the extended `NoesisInlines` bridge: re-apply,
//! `TextDecorations`, and `InlineUIContainer`, verified by reading back from
//! live Noesis objects (not echoing the spec), so a no-op apply, stale
//! collection, or dropped write fails.
//!
//! Font-free: only inline structure, decorations, and hosted-child identity
//! are asserted.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    InlineSpec, InlinesReadback, NoesisCamera, NoesisInlines, NoesisInlinesChanged, NoesisPlugin,
    NoesisView, TextDecorations, XamlRegistry,
};

const APPLY_AT_FRAME: usize = 10;
const REAPPLY_AT_FRAME: usize = 30;
const EXIT_AT_FRAME: usize = 70;

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
              mut changes: MessageReader<NoesisInlinesChanged>,
              mut exit: MessageWriter<AppExit>| {
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

fn noesis_license_from_env() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
}
