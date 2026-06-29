//! App-level system-integration bridge.
//!
//! Noesis raises a handful of **process-global host callbacks** (not per-view):
//! it asks the host to change the OS cursor, open a URL, or play a sound. The
//! runtime ([`noesis_runtime::integration`]) wraps each as a `set_*` registration
//! returning a `Drop`-guard. This plugin registers all three once, funnels their
//! firings through a shared queue, and surfaces them as Bevy messages:
//!
//!   * [`NoesisCursorRequested`] — fired when the engine wants a cursor change
//!     (e.g. the pointer moves over an element with a non-default `Cursor`). Also
//!     applied to the primary window's [`CursorIcon`] as a convenience.
//!   * [`NoesisOpenUrl`] — fired when a `Hyperlink` / command asks the host to
//!     open a URL, or when the app calls [`open_url`].
//!   * [`NoesisPlayAudio`] — fired when a `MediaElement` / sound asks the host to
//!     play audio, or when the app calls [`play_audio`].
//!
//! # Threading
//!
//! Noesis is thread-affine to the main thread in this crate (see
//! [`crate::render::NoesisRenderState`]), so every callback fires **synchronously
//! on the main thread** while the frame is driven in `PostUpdate`. The shared
//! queue is therefore only ever touched from one thread; the `Mutex` exists to
//! satisfy the runtime's `Send` bound on the closures, not to bridge threads.
//!
//! # Registration lifetime
//!
//! The three `*Callback` guards unregister via FFI on `Drop`, which crashes if
//! it runs after `shutdown()`. Bevy gives no drop order between main-world
//! resources, so the guards can't live in this plugin's own resource. Instead
//! [`install_integration_guards`] hands them to [`crate::render::NoesisRenderState`]
//! (via `own_integration_guards`), whose `Drop` releases them just before it
//! calls `shutdown()` — the same ownership discipline as the render device and
//! provider guards.
//!
//! These hooks are **single-slot, last-registration-wins** process-globally
//! (see the runtime module docs): adding this plugin twice, or mixing it with a
//! hand-rolled `set_cursor_callback`, means the last registration wins. We
//! register exactly once (guarded by a `Local` flag).

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::window::{CursorIcon, PrimaryWindow, SystemCursorIcon};

use noesis_runtime::integration;

use crate::render::{NoesisRenderState, NoesisSet};

/// Built-in cursor kind Noesis asks the host to display. Re-exported from the
/// runtime so consumers can match on [`NoesisCursorRequested::cursor`] without
/// depending on `noesis_runtime` directly.
pub use noesis_runtime::integration::CursorType;

// Re-export the engine-driving triggers: calling these synchronously invokes the
// registered callback, so they round-trip back out as the messages below.
pub use noesis_runtime::integration::{get_culture, open_url, play_audio, set_culture};

// ─────────────────────────────────────────────────────────────────────────────
// Messages
// ─────────────────────────────────────────────────────────────────────────────

/// Emitted when the engine requests a cursor change. Read with
/// `MessageReader<NoesisCursorRequested>`. The bridge also applies the request
/// to the primary window's [`CursorIcon`].
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoesisCursorRequested {
    /// The cursor the engine wants shown.
    pub cursor: CursorType,
}

/// Emitted when the engine asks the host to open a URL (e.g. a `Hyperlink`).
#[derive(Message, Debug, Clone, PartialEq, Eq)]
pub struct NoesisOpenUrl {
    /// The URL to open.
    pub url: String,
}

/// Emitted when the engine asks the host to play a sound.
#[derive(Message, Debug, Clone, PartialEq)]
pub struct NoesisPlayAudio {
    /// Canonicalized URI of the sound to play.
    pub uri: String,
    /// Requested volume in `[0.0, 1.0]`.
    pub volume: f32,
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared queue + registration guards
// ─────────────────────────────────────────────────────────────────────────────

/// One firing of a registered integration callback, waiting to be turned into a
/// Bevy message by [`drain_integration_queue`].
enum IntegrationEvent {
    Cursor(CursorType),
    OpenUrl(String),
    PlayAudio(String, f32),
}

/// Queue between the (main-thread) Noesis callbacks and the drain system. Cloned
/// `Arc` handles are captured by the registered closures.
#[derive(Resource, Clone, Default)]
struct SharedIntegrationQueue(Arc<Mutex<Vec<IntegrationEvent>>>);

impl SharedIntegrationQueue {
    fn push(&self, ev: IntegrationEvent) {
        self.0.lock().expect("integration queue poisoned").push(ev);
    }

    fn drain(&self) -> Vec<IntegrationEvent> {
        std::mem::take(&mut *self.0.lock().expect("integration queue poisoned"))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Systems
// ─────────────────────────────────────────────────────────────────────────────

/// Register the three process-global callbacks once and hand their guards to
/// [`NoesisRenderState`] for ordered teardown (see the module docs). Each
/// closure pushes onto the shared queue. Runs in [`NoesisSet::Sync`] — before
/// any view is built or driven — so the callbacks are live for the first frame.
#[allow(clippy::needless_pass_by_value)]
fn install_integration_guards(
    queue: Res<SharedIntegrationQueue>,
    state: Option<NonSendMut<NoesisRenderState>>,
    mut installed: Local<bool>,
) {
    if *installed {
        return;
    }
    let Some(mut state) = state else {
        return;
    };

    let cursor = {
        let q = queue.clone();
        integration::set_cursor_callback(move |_view, ty| {
            q.push(IntegrationEvent::Cursor(ty));
        })
    };
    let open_url = {
        let q = queue.clone();
        integration::set_open_url_callback(move |url| {
            q.push(IntegrationEvent::OpenUrl(url.to_string()));
        })
    };
    let play_audio = {
        let q = queue.clone();
        integration::set_play_audio_callback(move |uri, volume| {
            q.push(IntegrationEvent::PlayAudio(uri.to_string(), volume));
        })
    };

    state.own_integration_guards(vec![
        Box::new(cursor),
        Box::new(open_url),
        Box::new(play_audio),
    ]);
    *installed = true;
}

/// Drain queued callback firings into their corresponding Bevy messages. Runs
/// after [`NoesisSet::Drive`], so any callback raised while driving this frame's
/// view is surfaced the same frame.
#[allow(clippy::needless_pass_by_value)]
fn drain_integration_queue(
    queue: Res<SharedIntegrationQueue>,
    mut cursor: MessageWriter<NoesisCursorRequested>,
    mut open_url: MessageWriter<NoesisOpenUrl>,
    mut play_audio: MessageWriter<NoesisPlayAudio>,
) {
    for ev in queue.drain() {
        match ev {
            IntegrationEvent::Cursor(c) => {
                cursor.write(NoesisCursorRequested { cursor: c });
            }
            IntegrationEvent::OpenUrl(url) => {
                open_url.write(NoesisOpenUrl { url });
            }
            IntegrationEvent::PlayAudio(uri, volume) => {
                play_audio.write(NoesisPlayAudio { uri, volume });
            }
        }
    }
}

/// Apply the most recent cursor request to the primary window. No-op when there
/// is no window (e.g. a headless app) — the [`NoesisCursorRequested`] message is
/// still emitted for consumers that route the cursor themselves.
#[allow(clippy::needless_pass_by_value)]
fn apply_cursor_to_window(
    mut reader: MessageReader<NoesisCursorRequested>,
    window: Query<Entity, With<PrimaryWindow>>,
    mut commands: Commands,
) {
    let Ok(entity) = window.single() else {
        reader.clear();
        return;
    };
    // Only the last request in a frame matters.
    if let Some(req) = reader.read().last()
        && let Some(icon) = to_system_cursor(req.cursor)
    {
        commands.entity(entity).insert(CursorIcon::System(icon));
    }
}

/// Map a Noesis [`CursorType`] to the nearest Bevy [`SystemCursorIcon`].
/// Returns `None` for cursors with no standard system equivalent (`None`,
/// `Custom`, …) — those leave the window cursor unchanged.
fn to_system_cursor(ty: CursorType) -> Option<SystemCursorIcon> {
    use CursorType as C;
    Some(match ty {
        C::Arrow | C::ArrowCD | C::UpArrow | C::Pen => SystemCursorIcon::Default,
        C::No => SystemCursorIcon::NotAllowed,
        C::AppStarting => SystemCursorIcon::Progress,
        C::Cross => SystemCursorIcon::Crosshair,
        C::Help => SystemCursorIcon::Help,
        C::IBeam => SystemCursorIcon::Text,
        C::SizeAll | C::ScrollAll => SystemCursorIcon::Move,
        C::SizeNESW => SystemCursorIcon::NeswResize,
        C::SizeNS | C::ScrollNS | C::ScrollN | C::ScrollS => SystemCursorIcon::NsResize,
        C::SizeNWSE => SystemCursorIcon::NwseResize,
        C::SizeWE | C::ScrollWE | C::ScrollW | C::ScrollE => SystemCursorIcon::EwResize,
        C::ScrollNW => SystemCursorIcon::NwResize,
        C::ScrollNE => SystemCursorIcon::NeResize,
        C::ScrollSW => SystemCursorIcon::SwResize,
        C::ScrollSE => SystemCursorIcon::SeResize,
        C::Wait => SystemCursorIcon::Wait,
        C::Hand => SystemCursorIcon::Pointer,
        // `None`, `Custom`, and any future (`non_exhaustive`) variant have no
        // standard system equivalent — leave the window cursor unchanged.
        _ => return None,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

/// Registers Noesis's process-global integration callbacks and surfaces them as
/// Bevy messages. Added transitively by [`crate::NoesisPlugin`]. See the module
/// docs for threading and lifetime details.
pub struct NoesisIntegrationPlugin;

impl Plugin for NoesisIntegrationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SharedIntegrationQueue>()
            .add_message::<NoesisCursorRequested>()
            .add_message::<NoesisOpenUrl>()
            .add_message::<NoesisPlayAudio>()
            // Register the callbacks in `Sync` (before scenes are built/driven);
            // drain + apply after `Drive`, so a callback raised while driving a
            // view this frame is surfaced the same frame.
            .add_systems(
                PostUpdate,
                (
                    install_integration_guards.in_set(NoesisSet::Sync),
                    (drain_integration_queue, apply_cursor_to_window)
                        .chain()
                        .after(NoesisSet::Drive),
                ),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_maps_to_expected_system_icons() {
        assert_eq!(
            to_system_cursor(CursorType::Hand),
            Some(SystemCursorIcon::Pointer)
        );
        assert_eq!(
            to_system_cursor(CursorType::IBeam),
            Some(SystemCursorIcon::Text)
        );
        assert_eq!(
            to_system_cursor(CursorType::Cross),
            Some(SystemCursorIcon::Crosshair)
        );
        // No standard equivalent → leave the window cursor alone.
        assert_eq!(to_system_cursor(CursorType::None), None);
        assert_eq!(to_system_cursor(CursorType::Custom), None);
    }

    #[test]
    fn queue_drain_takes_all_and_resets() {
        let q = SharedIntegrationQueue::default();
        q.push(IntegrationEvent::Cursor(CursorType::Hand));
        q.push(IntegrationEvent::OpenUrl("u".into()));
        assert_eq!(q.drain().len(), 2);
        assert!(q.drain().is_empty());
    }
}
