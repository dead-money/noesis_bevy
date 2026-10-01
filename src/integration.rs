//! Noesis host callbacks (cursor, open URL, play audio) as Bevy messages.
//!
//! Noesis asks the host to change the OS cursor, open a URL, or play a sound
//! through process-global callbacks, not per view. [`NoesisIntegrationPlugin`]
//! registers all three and writes each firing as a message:
//!
//! - [`NoesisCursorRequested`]: the pointer moved over an element with a
//!   different `Cursor`. The plugin also sets the primary window's
//!   [`CursorIcon`], overwriting any icon you set there.
//! - [`NoesisOpenUrl`]: a `Hyperlink` or command asked to open a URL, or your
//!   code called [`open_url`].
//! - [`NoesisPlayAudio`]: XAML asked to play a sound, or your code called
//!   [`play_audio`].
//!
//! Messages are written after [`NoesisSet::Drive`], so a request raised
//! anywhere in the frame up to that point is readable the same frame from a
//! later `PostUpdate` system, or from `Update` on the next frame.
//!
//! The callbacks are registered during the first frame's
//! [`NoesisSet::Sync`]; [`open_url`] and [`play_audio`] calls made before that
//! do nothing. Each hook has one process-wide slot, so registering your own
//! callback with `noesis_runtime::integration::set_*_callback` replaces this
//! plugin's and its messages stop.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::window::{CursorIcon, PrimaryWindow, SystemCursorIcon};

use noesis_runtime::integration;

use crate::render::{NoesisRenderState, NoesisSet};

/// Built-in cursor kind Noesis asks the host to display, carried by
/// [`NoesisCursorRequested::cursor`].
pub use noesis_runtime::integration::CursorType;

pub use noesis_runtime::integration::{get_culture, open_url, play_audio, set_culture};

/// Noesis wants a different cursor shown. The plugin also applies it to the
/// primary window's [`CursorIcon`] when there is a system equivalent.
#[derive(Message, Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoesisCursorRequested {
    /// The cursor the engine wants shown.
    pub cursor: CursorType,
}

/// Noesis asks the host to open a URL, for example from a `Hyperlink`. The
/// plugin doesn't open it; handle the message yourself.
#[derive(Message, Debug, Clone, PartialEq, Eq)]
pub struct NoesisOpenUrl {
    /// The URL to open.
    pub url: String,
}

/// Noesis asks the host to play a sound. The plugin doesn't play it; handle
/// the message yourself.
#[derive(Message, Debug, Clone, PartialEq)]
pub struct NoesisPlayAudio {
    /// URI of the sound to play.
    pub uri: String,
    /// Requested volume in `[0.0, 1.0]`.
    pub volume: f32,
}

enum IntegrationEvent {
    Cursor(CursorType),
    OpenUrl(String),
    PlayAudio(String, f32),
}

// Only touched on the main thread; the `Mutex` satisfies the callbacks' `Send` bound.
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

/// Registers the callbacks once, before any view is built. The guards go to
/// [`NoesisRenderState`] because unregistering after `shutdown()` crashes, and
/// Bevy gives no drop order between resources; its `Drop` releases them just
/// before calling `shutdown()`.
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

/// Applies the frame's last cursor request to the primary window, if any.
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
    if let Some(req) = reader.read().last()
        && let Some(icon) = to_system_cursor(req.cursor)
    {
        commands.entity(entity).insert(CursorIcon::System(icon));
    }
}

/// Nearest Bevy [`SystemCursorIcon`], or `None` (leave the cursor alone) when
/// there is no system equivalent.
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
        _ => return None,
    })
}

/// Registers the Noesis host callbacks and writes them as
/// [`NoesisCursorRequested`], [`NoesisOpenUrl`] and [`NoesisPlayAudio`]
/// messages. [`NoesisPlugin`](crate::NoesisPlugin) adds it.
pub struct NoesisIntegrationPlugin;

impl Plugin for NoesisIntegrationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SharedIntegrationQueue>()
            .add_message::<NoesisCursorRequested>()
            .add_message::<NoesisOpenUrl>()
            .add_message::<NoesisPlayAudio>()
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
