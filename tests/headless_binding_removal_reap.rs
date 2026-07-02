//! Component-removal reap regression for [`NoesisBinding`] (audit P0.9
//! completion). Removing the binding component from a *live* view must detach the
//! live XAML-wired binding off its target DP (via `ClearBinding`) and drain the
//! render-side entries — symmetrically with entity-despawn teardown — not leave
//! the binding driving the property forever.
//!
//! `NoesisBinding` was the one bridge P0.9 excluded, because stopping a live
//! binding needed the `clear_binding` FFI that has since landed. This drives a
//! view whose binding upper-cases `Source.Text` into `Upper.Text` until it is
//! live (`live_bindings == 1`, `Upper.Text == "HELLO"`), then `remove::<NoesisBinding>()`
//! while keeping the view alive. After removal it mutates the source and asserts
//! the target no longer follows (`Upper.Text != "WORLD"`) and the side table
//! drained (`live_bindings == 0`) with the scene still live.
//!
//! Font-free XAML; no glyph rendering involved.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    ConvertArg, Converted, DpKind, DpValue, NoesisBinding, NoesisCamera, NoesisDiagnostics,
    NoesisDp, NoesisDpChanged, NoesisView, SourceSpec, XamlRegistry,
};

mod common;
use common::{headless_app, run_until};

// Stimulus sequence: settle the live binding, drop it, mutate the source past the
// reap, then snapshot. The run's exit is the terminal post-removal predicate.
const CAPTURE_PRE_AT: usize = 25;
const REMOVE_AT: usize = 26;
const MUTATE_AT: usize = 30;
const CAPTURE_POST_AT: usize = 50;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="200" Height="80">
  <StackPanel>
    <TextBlock x:Name="Source"/>
    <TextBlock x:Name="Upper"/>
  </StackPanel>
</Grid>"##;

type Observed = Vec<(Entity, String, String, DpValue)>;

fn upper_binding() -> NoesisBinding {
    NoesisBinding::new().converted(
        "Upper",
        "Text",
        SourceSpec::element("Source", "Text"),
        |v: &ConvertArg, _p: &ConvertArg| Some(Converted::String(v.as_str()?.to_uppercase())),
    )
}

#[test]
fn removing_binding_from_a_live_view_reaps_and_stops_it() {
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let pre: Arc<Mutex<Option<(usize, usize)>>> = Arc::new(Mutex::new(None));
    let post: Arc<Mutex<Option<(usize, usize)>>> = Arc::new(Mutex::new(None));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "binding_reap.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "binding_reap.xaml".to_string(),
                        size: UVec2::new(200, 80),
                        ..default()
                    },
                    upper_binding(),
                    // Drive the source element directly, and watch the target the
                    // binding feeds. The watch stays after NoesisBinding is removed.
                    NoesisDp::new().set_string("Source", "Text", "hello").watch(
                        "Upper",
                        "Text",
                        DpKind::Str,
                    ),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let observed_sys = Arc::clone(&observed);
    let pre_sys = Arc::clone(&pre);
    let post_sys = Arc::clone(&post);
    let view_update = Arc::clone(&view_entity);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut commands: Commands,
              diag: Res<NoesisDiagnostics>,
              mut dps: Query<&mut NoesisDp>,
              mut changes: MessageReader<NoesisDpChanged>| {
            *frame += 1;
            for ev in changes.read() {
                observed_sys.lock().unwrap().push((
                    ev.view,
                    ev.name.clone(),
                    ev.property.clone(),
                    ev.value.clone(),
                ));
            }
            if *frame == CAPTURE_PRE_AT {
                *pre_sys.lock().unwrap() = Some((diag.live_bindings, diag.live_scenes));
            }
            if *frame == REMOVE_AT
                && let Some(view) = *view_update.lock().unwrap()
            {
                // Drop only the binding; the view (and scene) stay live. The reap
                // must run off RemovedComponents<NoesisBinding>, not despawn.
                commands.entity(view).remove::<NoesisBinding>();
            }
            if *frame == MUTATE_AT {
                // Source mutation *after* the reap: a still-live binding would
                // push "WORLD" to Upper; a detached one leaves it be.
                for mut dp in &mut dps {
                    dp.write_string("Source", "Text", "world");
                }
            }
            if *frame == CAPTURE_POST_AT {
                *post_sys.lock().unwrap() = Some((diag.live_bindings, diag.live_scenes));
            }
        },
    );

    // Exit once the post-removal snapshot has been taken and shows the binding
    // entries drained while the scene stayed live.
    let pred_post = Arc::clone(&post);
    let reaped = run_until(
        &mut app,
        240,
        move |_app| matches!(*pred_post.lock().unwrap(), Some((bindings, scenes)) if bindings == 0 && scenes == 1),
    );

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    let (pre_bindings, pre_scenes) = pre.lock().unwrap().expect("pre-removal snapshot captured");
    let (post_bindings, post_scenes) = post
        .lock()
        .unwrap()
        .expect("post-removal snapshot captured");
    eprintln!(
        "--- binding reap pre=(bindings={pre_bindings}, scenes={pre_scenes}) \
         post=(bindings={post_bindings}, scenes={post_scenes}) ---"
    );

    let latest = |name: &str, prop: &str| -> Option<DpValue> {
        got.iter()
            .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
            .map(|(_, _, _, v)| v.clone())
    };

    assert!(
        reaped,
        "binding entries never drained to 0 (with the scene still live) within \
         240 frames; pre=(bindings={pre_bindings}, scenes={pre_scenes})",
    );

    // Control: the binding delivered "HELLO" while it was live.
    assert!(
        got.iter().any(|(e, n, p, v)| *e == view
            && n == "Upper"
            && p == "Text"
            && *v == DpValue::Str("HELLO".to_string())),
        "binding should have upper-cased Source.Text to \"HELLO\" before removal",
    );
    assert_eq!(
        pre_bindings, 1,
        "binding entry should be live before removal"
    );
    assert_eq!(pre_scenes, 1, "view scene should be live before removal");

    // After removal the source mutation must NOT reach the target.
    assert_ne!(
        latest("Upper", "Text"),
        Some(DpValue::Str("WORLD".to_string())),
        "source mutation still propagated after NoesisBinding removal",
    );
    // Side table drained, scene kept: this is the component-removal path.
    assert_eq!(
        post_bindings, 0,
        "removing NoesisBinding must drain its binding entries; {post_bindings} still tracked",
    );
    assert_eq!(
        post_scenes, 1,
        "removing NoesisBinding must NOT tear down the view's scene; {post_scenes} live scenes",
    );
}
