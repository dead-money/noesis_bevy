//! F5b regression: a [`UiPanel`] fragment that is malformed but *loadable* (Noesis
//! returns a partial tree and only warns) is surfaced as a Bevy `error!` naming the
//! panel entity and URI, instead of a silent half-render.
//!
//! What this asserts: no panic, the malformed fragment still builds a `PanelEntry`
//! (`live_panels == 2`, distinguishing the lenient-parse path from F5's hard
//! `None` case), and a valid sibling panel is unaffected. The `error!` surfacing
//! is exercised by this path (a tag-mismatch fragment); see the run log.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    NoesisCamera, NoesisDiagnostics, NoesisPanelAppExt, NoesisPanelText, NoesisPanelTextChanged,
    NoesisPlugin, NoesisView, NoesisViewModel, UiPanel, XamlRegistry,
};
use tracing::Subscriber;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

/// Collects ERROR-level tracing messages so the test can assert F5b surfaced one.
struct ErrorCapture(Arc<Mutex<Vec<String>>>);
impl<S: Subscriber> Layer<S> for ErrorCapture {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        if *event.metadata().level() != tracing::Level::ERROR {
            return;
        }
        struct V<'a>(&'a mut String);
        impl tracing::field::Visit for V<'_> {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                if f.name() == "message" {
                    use std::fmt::Write;
                    let _ = write!(self.0, "{v:?}");
                }
            }
        }
        let mut msg = String::new();
        event.record(&mut V(&mut msg));
        self.0.lock().unwrap().push(msg);
    }
}

const HOST_XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="256" Height="256">
  <StackPanel x:Name="Hud"/>
</Grid>"##;

const GOOD_XAML: &str = r##"<StackPanel xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml">
  <TextBlock x:Name="GoodText" Text="{Binding Health}"/>
</StackPanel>"##;

// Malformed but loadable: the `StackPanel` is closed with `</Grid>`. Noesis warns
// (XamlParser tag mismatch) but still returns a partial element.
const BAD_XAML: &str = r##"<StackPanel xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml">
  <TextBlock x:Name="Broken" Text="{Binding Health}"/>
</Grid>"##;

#[derive(Component, NoesisViewModel)]
struct Health(f32);

const EXIT_AT: usize = 48;

#[test]
fn malformed_fragment_loads_partial_and_is_surfaced() {
    noesis_license_from_env();

    let captured: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
    let final_live: Arc<Mutex<usize>> = Arc::new(Mutex::new(usize::MAX));
    let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    // Capture ERROR events on this thread (where the NonSend reconcile runs) so we
    // can assert F5b actually logged. Disable Bevy's LogPlugin so ours is the sink.
    let _log_guard = tracing::subscriber::set_default(
        tracing_subscriber::registry().with(ErrorCapture(Arc::clone(&errors))),
    );

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .build()
            .disable::<bevy::log::LogPlugin>()
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
    app.add_noesis_panel_field::<Health>();

    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            for (name, xaml) in [
                ("host.xaml", HOST_XAML),
                ("good.xaml", GOOD_XAML),
                ("bad.xaml", BAD_XAML),
            ] {
                reg.insert(name.to_string(), Arc::new(xaml.as_bytes().to_vec()));
            }

            let host = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "host.xaml".to_string(),
                        size: UVec2::new(256, 256),
                        ..default()
                    },
                ))
                .id();

            commands.spawn((
                UiPanel::new("bad.xaml").mount_into(host, "Hud"),
                Health(1.0),
            ));
            commands.spawn((
                UiPanel::new("good.xaml").mount_into(host, "Hud"),
                NoesisPanelText::new().watching(["GoodText"]),
                Health(42.0),
            ));
        },
    );

    let captured_sys = Arc::clone(&captured);
    let final_sys = Arc::clone(&final_live);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              diag: Res<NoesisDiagnostics>,
              mut reads: MessageReader<NoesisPanelTextChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;
            for ev in reads.read() {
                captured_sys
                    .lock()
                    .unwrap()
                    .insert(ev.name.clone(), ev.text.clone());
            }
            if *frame >= EXIT_AT {
                *final_sys.lock().unwrap() = diag.live_panels;
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let good = captured.lock().unwrap().clone();
    let live = *final_live.lock().unwrap();

    // Both built a PanelEntry: the malformed one still LOADS (partial tree), unlike
    // F5's missing-URI case where load returns None and the panel never mounts.
    assert_eq!(
        live, 2,
        "expected both panels to mount (malformed fragment loads as a partial tree); got {live}",
    );
    assert_eq!(
        good.get("GoodText").map(String::as_str),
        Some("42"),
        "the valid sibling's binding did not reach the UI; reads {good:?}",
    );

    // F5b: the malformed fragment's parser warning surfaced as a Bevy error! naming
    // the panel's URI, instead of vanishing into the Noesis log.
    let errs = errors.lock().unwrap().clone();
    assert!(
        errs.iter()
            .any(|e| e.contains("bad.xaml") && e.contains("parser warning")),
        "expected an F5b error! surfacing the malformed fragment; captured errors: {errs:?}",
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
