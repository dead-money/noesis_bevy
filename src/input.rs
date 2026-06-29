//! Bevy → Noesis input forwarding.
//!
//! The main app observes Bevy's raw input events and a few window events,
//! converts each into a [`NoesisInputEvent`], and pushes it onto a shared
//! [`NoesisInputQueue`] resource. The queue is cloned into the render world
//! via [`ExtractResource`] each frame; the render-side `apply_noesis_input`
//! system (defined in `render.rs`) drains it onto the live
//! [`noesis_runtime::view::View`] just before the frame is driven.
//!
//! # Coordinate handling
//!
//! Bevy delivers cursor positions in *logical* pixels relative to the
//! window. Noesis hit-tests in the view's own pixel space, whatever
//! [`NoesisView::size`] is set to (the intermediate texture).
//! We convert on the main side, collapsing Window scale factor and any
//! intermediate-vs-window size mismatch into a single ratio:
//!
//! ```text
//!   view_x = cursor_logical_x * view_w / window_logical_w
//!   view_y = cursor_logical_y * view_h / window_logical_h
//! ```
//!
//! Once `resize_noesis_scene` snaps the intermediate to the window's
//! physical size, this ratio reduces to the scale factor.
//!
//! # Queue lifecycle
//!
//! Systems that push run in `PreUpdate`. The resource is cloned to the
//! render world by [`ExtractResourcePlugin`] between the main schedule
//! and the render sub-app's own schedules. Since Bevy's `Last` runs
//! *before* the render sub-app's `ExtractSchedule` (the main schedule
//! completes, then sub-apps run), we can't clear in `Last`; that would
//! wipe the queue before extract copies it. Instead we clear at the
//! very start of the next frame's `PreUpdate`, before the forwarders
//! push new events:
//!
//! ```text
//!   Frame N PreUpdate:  clear (drops N-1 events already extracted)
//!                       push frame-N events A, B, C
//!   Frame N Last:       (no-op)
//!   Between N and N+1:  ExtractSchedule copies [A, B, C] into render world
//!   Render frame N:     apply_noesis_input drains render-side copy
//!   Frame N+1 PreUpdate: clear (drops A, B, C from main queue), then push again
//! ```
//!
//! `Clone` on the queue is cheap: every variant is `Copy`-sized, so
//! extract is a `Vec` clone.

use bevy::input::{
    ButtonState,
    keyboard::KeyboardInput,
    mouse::{MouseButton as BevyMouseButton, MouseButtonInput, MouseScrollUnit, MouseWheel},
    touch::{TouchInput, TouchPhase},
};
use bevy::prelude::*;
use bevy::window::{CursorMoved, PrimaryWindow, WindowFocused, WindowResized};
use bevy_render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use noesis_runtime::view::{Key, MouseButton};

use crate::render::NoesisView;

pub mod key_map;

// ── Events and queue ───────────────────────────────────────────────────────

/// A single input event already translated into Noesis terms, waiting in the
/// [`NoesisInputQueue`] to be replayed onto the live [`View`].
///
/// All `x`/`y` coordinates are in the view's own pixel space (see the
/// module-level coordinate handling notes), already converted from Bevy's
/// logical-pixel window coordinates by `to_view_coords`.
///
/// [`View`]: noesis_runtime::view::View
#[derive(Clone, Copy, Debug)]
pub enum NoesisInputEvent {
    /// Pointer moved to a new position. Noesis tracks this as the last known
    /// cursor location for subsequent hit-tests.
    MouseMove {
        /// X position in view-pixel space.
        x: i32,
        /// Y position in view-pixel space.
        y: i32,
    },
    /// A mouse button changed state at the given position.
    MouseButton {
        /// `true` on press, `false` on release.
        down: bool,
        /// X position in view-pixel space.
        x: i32,
        /// Y position in view-pixel space.
        y: i32,
        /// Which button changed.
        button: MouseButton,
    },
    /// A wheel detent, in the Win32 `WHEEL_DELTA` convention Noesis expects
    /// (120 units per notch).
    MouseWheel {
        /// X position in view-pixel space.
        x: i32,
        /// Y position in view-pixel space.
        y: i32,
        /// Wheel movement in 120-units-per-notch increments.
        delta: i32,
    },
    /// A scroll in line counts, the path Noesis's scrolling controls listen
    /// on. Emitted alongside [`MouseWheel`](Self::MouseWheel) so controls
    /// bound to either signal respond.
    Scroll {
        /// X position in view-pixel space.
        x: i32,
        /// Y position in view-pixel space.
        y: i32,
        /// Scroll amount in lines.
        value: f32,
        /// `true` for horizontal scroll, `false` for vertical.
        horizontal: bool,
    },
    /// A touch point made contact.
    TouchDown {
        /// X position in view-pixel space.
        x: i32,
        /// Y position in view-pixel space.
        y: i32,
        /// Touch point identifier, stable across this contact's lifetime.
        id: u64,
    },
    /// A touch point moved while in contact.
    TouchMove {
        /// X position in view-pixel space.
        x: i32,
        /// Y position in view-pixel space.
        y: i32,
        /// Touch point identifier, stable across this contact's lifetime.
        id: u64,
    },
    /// A touch point lifted or was canceled.
    TouchUp {
        /// X position in view-pixel space.
        x: i32,
        /// Y position in view-pixel space.
        y: i32,
        /// Touch point identifier, stable across this contact's lifetime.
        id: u64,
    },
    /// A key was pressed. Carries the mapped Noesis [`Key`]; keys that don't
    /// map are dropped before they reach the queue.
    KeyDown(Key),
    /// A key was released.
    KeyUp(Key),
    /// A typed character, as a Unicode scalar value. Drives text entry
    /// separately from the [`KeyDown`](Self::KeyDown) / [`KeyUp`](Self::KeyUp)
    /// pair, including on auto-repeat.
    Char(u32),
    /// Window focus changed: `true` gained, `false` lost.
    Focus(bool),
}

/// Batched input events waiting to be drained onto the Noesis `View`.
/// Populated by systems in this module; drained by the render-world
/// `apply_noesis_input` system every frame.
#[derive(Resource, ExtractResource, Clone, Default, Debug)]
pub struct NoesisInputQueue {
    /// Events queued this frame, in arrival order.
    pub events: Vec<NoesisInputEvent>,
}

impl NoesisInputQueue {
    /// Append an event to the back of the queue.
    pub fn push(&mut self, ev: NoesisInputEvent) {
        self.events.push(ev);
    }

    /// Drain every queued event, leaving the queue empty. The render-side
    /// `apply_noesis_input` system uses this to feed events onto the [`View`].
    ///
    /// [`View`]: noesis_runtime::view::View
    pub fn drain(&mut self) -> std::vec::Drain<'_, NoesisInputEvent> {
        self.events.drain(..)
    }
}

/// Scale a logical-px point on the window into Noesis view-pixel space.
/// Returns `None` when the window has zero size (startup race).
fn to_view_coords(window: &Window, scene: &NoesisView, x: f32, y: f32) -> Option<(i32, i32)> {
    let ww = window.width();
    let wh = window.height();
    if ww <= 0.0 || wh <= 0.0 {
        return None;
    }
    let vw = scene.size.x as f32;
    let vh = scene.size.y as f32;
    Some(((x * vw / ww) as i32, (y * vh / wh) as i32))
}

// ── Systems ────────────────────────────────────────────────────────────────

/// Track the cursor position separately from `CursorMoved` so we can attach
/// a move-at-press-coord to every `MouseButton` / Touch event. Noesis
/// hit-tests on the last known pointer position; without this, a button
/// pressed before the cursor has entered the window hits (0,0).
#[derive(Resource, Default, Clone, Copy, Debug)]
struct LastPointer {
    x: i32,
    y: i32,
    valid: bool,
}

#[allow(clippy::needless_pass_by_value)]
fn forward_cursor_moved(
    mut reader: MessageReader<CursorMoved>,
    mut queue: ResMut<NoesisInputQueue>,
    mut last: ResMut<LastPointer>,
    window: Single<&Window, With<PrimaryWindow>>,
    views: Query<&NoesisView>,
) {
    // First view only; no per-view pointer routing yet.
    let Some(scene) = views.iter().next() else {
        reader.read(); // drop events so we don't replay them later
        return;
    };
    for ev in reader.read() {
        if let Some((x, y)) = to_view_coords(&window, scene, ev.position.x, ev.position.y) {
            last.x = x;
            last.y = y;
            last.valid = true;
            queue.push(NoesisInputEvent::MouseMove { x, y });
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn forward_mouse_buttons(
    mut reader: MessageReader<MouseButtonInput>,
    mut queue: ResMut<NoesisInputQueue>,
    last: Res<LastPointer>,
) {
    for ev in reader.read() {
        let button = match ev.button {
            BevyMouseButton::Left => MouseButton::Left,
            BevyMouseButton::Right => MouseButton::Right,
            BevyMouseButton::Middle => MouseButton::Middle,
            BevyMouseButton::Back => MouseButton::XButton1,
            BevyMouseButton::Forward => MouseButton::XButton2,
            BevyMouseButton::Other(_) => continue,
        };
        let (x, y) = if last.valid { (last.x, last.y) } else { (0, 0) };
        // Re-enqueue last pos so the press coord matches the last MouseMove,
        // regardless of event arrival order.
        if last.valid {
            queue.push(NoesisInputEvent::MouseMove { x, y });
        }
        queue.push(NoesisInputEvent::MouseButton {
            down: matches!(ev.state, ButtonState::Pressed),
            x,
            y,
            button,
        });
    }
}

#[allow(clippy::needless_pass_by_value)]
fn forward_mouse_wheel(
    mut reader: MessageReader<MouseWheel>,
    mut queue: ResMut<NoesisInputQueue>,
    last: Res<LastPointer>,
) {
    // Emit both a Noesis MouseWheel event (Windows-style, 120 units per
    // detent) and a Scroll event (line count) so controls listening to
    // either get the signal. Redundant calls are cheap.
    for ev in reader.read() {
        let (x, y) = if last.valid { (last.x, last.y) } else { (0, 0) };
        // Convert pixel scroll to "lines" for the Scroll path: rough
        // heuristic of 40 px/line; MouseScrollUnit::Line passes through.
        let lines_y = match ev.unit {
            MouseScrollUnit::Line => ev.y,
            MouseScrollUnit::Pixel => ev.y / 40.0,
        };
        let lines_x = match ev.unit {
            MouseScrollUnit::Line => ev.x,
            MouseScrollUnit::Pixel => ev.x / 40.0,
        };
        // 120 units per line is the Win32 `WHEEL_DELTA` convention Noesis uses.
        let wheel_delta = (lines_y * 120.0) as i32;
        if wheel_delta != 0 {
            queue.push(NoesisInputEvent::MouseWheel {
                x,
                y,
                delta: wheel_delta,
            });
        }
        if lines_y != 0.0 {
            queue.push(NoesisInputEvent::Scroll {
                x,
                y,
                value: lines_y,
                horizontal: false,
            });
        }
        if lines_x != 0.0 {
            queue.push(NoesisInputEvent::Scroll {
                x,
                y,
                value: lines_x,
                horizontal: true,
            });
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn forward_keyboard(mut reader: MessageReader<KeyboardInput>, mut queue: ResMut<NoesisInputQueue>) {
    for ev in reader.read() {
        // Skip synthetic repeat events; Noesis runs its own repeat timing
        // off the logical KeyDown/KeyUp pair.
        if ev.repeat {
            // Still emit Char on repeat so TextBox auto-repeat works.
            if matches!(ev.state, ButtonState::Pressed)
                && let Some(text) = ev.text.as_deref()
            {
                for ch in text.chars() {
                    queue.push(NoesisInputEvent::Char(ch as u32));
                }
            }
            continue;
        }
        let key = key_map::from_bevy(ev.key_code);
        match ev.state {
            ButtonState::Pressed => {
                if key != Key::None {
                    queue.push(NoesisInputEvent::KeyDown(key));
                }
                if let Some(text) = ev.text.as_deref() {
                    for ch in text.chars() {
                        queue.push(NoesisInputEvent::Char(ch as u32));
                    }
                }
            }
            ButtonState::Released => {
                if key != Key::None {
                    queue.push(NoesisInputEvent::KeyUp(key));
                }
            }
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn forward_touch(
    mut reader: MessageReader<TouchInput>,
    mut queue: ResMut<NoesisInputQueue>,
    window: Single<&Window, With<PrimaryWindow>>,
    views: Query<&NoesisView>,
) {
    let Some(scene) = views.iter().next() else {
        reader.read();
        return;
    };
    for ev in reader.read() {
        let Some((x, y)) = to_view_coords(&window, scene, ev.position.x, ev.position.y) else {
            continue;
        };
        let id = ev.id;
        match ev.phase {
            TouchPhase::Started => queue.push(NoesisInputEvent::TouchDown { x, y, id }),
            TouchPhase::Moved => queue.push(NoesisInputEvent::TouchMove { x, y, id }),
            TouchPhase::Ended | TouchPhase::Canceled => {
                queue.push(NoesisInputEvent::TouchUp { x, y, id });
            }
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn forward_focus(mut reader: MessageReader<WindowFocused>, mut queue: ResMut<NoesisInputQueue>) {
    for ev in reader.read() {
        queue.push(NoesisInputEvent::Focus(ev.focused));
    }
}

/// Snap each [`NoesisView`]'s size to the window's physical pixel size on
/// resize. Makes the `NoesisNode` blit effectively 1:1 and brings the
/// cursor-coord ratio in `to_view_coords` down to just the scale factor.
///
/// Runs on the main app: the render-world clone of each [`NoesisView`] is
/// overwritten each frame via `ExtractComponent`, so the source of truth has to
/// live here. The render side picks up the new size on the next frame's
/// `ensure_scene`, which detects the mismatch and rebuilds the intermediate
/// texture + re-calls `View::set_size`.
#[allow(clippy::needless_pass_by_value)]
fn resize_noesis_scene(
    mut reader: MessageReader<WindowResized>,
    mut views: Query<&mut NoesisView>,
    window: Single<&Window, With<PrimaryWindow>>,
) {
    if views.is_empty() {
        reader.read();
        return;
    }
    for _ev in reader.read() {
        let physical = window.physical_size();
        if physical.x > 0 && physical.y > 0 {
            for mut scene in &mut views {
                scene.size = UVec2::new(physical.x, physical.y);
            }
        }
    }
}

/// Clear the main-app queue at the start of `PreUpdate`, before the
/// forwarders push new events. By the time this fires, the render
/// sub-app's extract has already copied whatever the previous frame
/// queued; see the module-level queue-lifecycle diagram.
fn clear_queue_before_push(mut queue: ResMut<NoesisInputQueue>) {
    queue.events.clear();
}

// ── Plugin ─────────────────────────────────────────────────────────────────

/// Installs the Bevy → Noesis input bridge. Add alongside [`NoesisPlugin`].
///
/// [`NoesisPlugin`]: crate::NoesisPlugin
pub struct NoesisInputPlugin;

impl Plugin for NoesisInputPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NoesisInputQueue>()
            .init_resource::<LastPointer>()
            .add_plugins(ExtractResourcePlugin::<NoesisInputQueue>::default())
            .add_systems(
                PreUpdate,
                (
                    // MUST run first: resets the queue so this frame's
                    // pushes land in an empty buffer. See the
                    // queue-lifecycle diagram in the module docs.
                    clear_queue_before_push,
                    resize_noesis_scene,
                    forward_cursor_moved,
                    forward_mouse_buttons,
                    forward_mouse_wheel,
                    forward_keyboard,
                    forward_touch,
                    forward_focus,
                )
                    .chain(),
            );
    }
}
