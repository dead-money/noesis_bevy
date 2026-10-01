//! Forwards Bevy mouse, keyboard, touch and window input to Noesis.
//!
//! [`NoesisInputPlugin`] (added by [`NoesisPlugin`](crate::NoesisPlugin))
//! runs forwarders in `PreUpdate` that translate Bevy input messages into
//! [`NoesisInputEvent`]s on the [`NoesisInputQueue`] resource. A system in
//! [`NoesisSet::Apply`](crate::NoesisSet::Apply) (`PostUpdate`) drains the
//! queue onto the live Noesis views in the same frame. Everything runs in
//! the main world, on the thread Noesis is pinned to.
//!
//! You can push your own events onto [`NoesisInputQueue`] (for example from
//! a gamepad), and read [`NoesisPointerOverUi`] to keep clicks on the UI from
//! reaching the game world.
//!
//! # Views and windows
//!
//! Only the primary window is forwarded. Pointer and touch positions are
//! converted against the primary view, the [`NoesisView`] with the lowest
//! `Entity`, and keyboard and focus events also go to that view. A window
//! resize sets every [`NoesisView::size`] to the window's physical size.
//!
//! # Coordinates
//!
//! Bevy reports cursor positions in logical pixels relative to the window.
//! Noesis hit-tests in the view's own pixel space ([`NoesisView::size`]). The
//! forwarders scale by one ratio that covers both the window scale factor and
//! any view-vs-window size mismatch:
//!
//! ```text
//!   view_x = cursor_logical_x * view_w / window_logical_w
//!   view_y = cursor_logical_y * view_h / window_logical_h
//! ```
//!
//! After a resize snaps the view to the window's physical size, the ratio is
//! the scale factor.

use bevy::input::{
    ButtonState,
    keyboard::KeyboardInput,
    mouse::{MouseButton as BevyMouseButton, MouseButtonInput, MouseScrollUnit, MouseWheel},
    touch::{TouchInput, TouchPhase},
};
use bevy::prelude::*;
use bevy::window::{CursorLeft, CursorMoved, PrimaryWindow, WindowFocused, WindowResized};
use noesis_runtime::view::{Key, MouseButton};

use crate::render::NoesisView;

pub mod key_map;

/// A single input event already translated into Noesis terms, waiting in the
/// [`NoesisInputQueue`] to be replayed onto the live [`View`].
///
/// All `x`/`y` coordinates are in the view's own pixel space (see
/// [Coordinates](self#coordinates)). Push them yourself in that space.
///
/// [`View`]: noesis_runtime::view::View
#[derive(Clone, Copy, Debug)]
pub enum NoesisInputEvent {
    /// Pointer moved. Noesis hit-tests later button and wheel events against
    /// the last move.
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
    /// Vertical wheel movement, in the Win32 `WHEEL_DELTA` convention Noesis
    /// expects (120 units per notch). Positive scrolls up.
    MouseWheel {
        /// X position in view-pixel space.
        x: i32,
        /// Y position in view-pixel space.
        y: i32,
        /// Wheel movement in 120-units-per-notch increments.
        delta: i32,
    },
    /// Horizontal wheel movement (tilt wheel or trackpad swipe), in the same
    /// 120-units-per-notch convention as [`MouseWheel`](Self::MouseWheel).
    /// Positive scrolls right.
    MouseHWheel {
        /// X position in view-pixel space.
        x: i32,
        /// Y position in view-pixel space.
        y: i32,
        /// Wheel movement in 120-units-per-notch increments.
        delta: i32,
    },
    /// A scroll in lines, the other input Noesis scrolling controls listen
    /// on. The bridge never emits it: the mouse wheel drives only
    /// [`MouseWheel`](Self::MouseWheel) and [`MouseHWheel`](Self::MouseHWheel),
    /// so a `ScrollViewer` doesn't scroll twice. Push it from your own scroll
    /// source, such as a gamepad.
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
    /// A key was pressed. OS auto-repeat arrives as repeated `KeyDown`s. Keys
    /// that map to [`Key::None`] are dropped before they reach the queue.
    KeyDown(Key),
    /// A key was released.
    KeyUp(Key),
    /// A typed character, as a Unicode scalar value. Text entry uses this, not
    /// [`KeyDown`](Self::KeyDown); auto-repeat produces one per repeat.
    Char(u32),
    /// Window focus changed: `true` gained, `false` lost.
    Focus(bool),
}

/// A [`NoesisInputEvent`] paired with the view it is routed to.
///
/// The pointer forwarders stamp the view whose pixel space they converted the
/// position into, so the event hit-tests against that view. `target: None`
/// means the primary view (the live view with the lowest `Entity`), used for
/// keyboard and focus events and by [`NoesisInputQueue::push`]. Events whose
/// target has no live view are dropped.
///
/// The built-in forwarders only target the primary view; there is no per-view
/// pointer routing yet.
#[derive(Clone, Copy, Debug)]
pub struct TargetedInput {
    /// The view this event is routed to, or `None` for the primary view.
    pub target: Option<Entity>,
    /// The translated input event.
    pub event: NoesisInputEvent,
}

/// Input events waiting to be delivered to Noesis.
///
/// The forwarders fill it in `PreUpdate`, and a system in
/// [`NoesisSet::Apply`](crate::NoesisSet::Apply) drains it every frame. Push
/// from a system that runs before that set to have the event delivered in
/// the same frame.
#[derive(Resource, Clone, Default, Debug)]
pub struct NoesisInputQueue {
    /// Events queued this frame, in arrival order, each tagged with its target
    /// view (see [`TargetedInput`]).
    pub events: Vec<TargetedInput>,
}

/// Whether the mouse pointer is currently over hit-test-visible Noesis UI.
///
/// Read it to keep clicks on the UI from reaching the game world. It holds the
/// hit-test result of the last mouse move, button or touch event; a wheel or
/// scroll event the UI handles also sets it. It updates in
/// [`NoesisSet::Apply`](crate::NoesisSet::Apply) (`PostUpdate`), so `Update`
/// systems see the previous frame's value. It resets when the cursor leaves
/// the primary window or no view is live.
#[derive(Resource, Default, Clone, Copy, Debug)]
pub struct NoesisPointerOverUi {
    /// `true` when the pointer is over hit-test-visible UI.
    pub over: bool,
}

impl NoesisInputQueue {
    /// Append an event bound to the primary view (see [`TargetedInput`]).
    pub fn push(&mut self, ev: NoesisInputEvent) {
        self.push_to_opt(None, ev);
    }

    /// Append an event bound to a specific view.
    pub fn push_to(&mut self, target: Entity, ev: NoesisInputEvent) {
        self.push_to_opt(Some(target), ev);
    }

    /// Append an event bound to `target` when `Some`, or to the primary view
    /// when `None`.
    pub fn push_to_opt(&mut self, target: Option<Entity>, ev: NoesisInputEvent) {
        self.events.push(TargetedInput { target, event: ev });
    }

    /// Removes and returns every queued event in arrival order.
    pub fn drain(&mut self) -> std::vec::Drain<'_, TargetedInput> {
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

/// Last converted cursor position. Button events replay it as a move first
/// because Noesis hit-tests on the last move; wheel events use it as their
/// position. Before the cursor enters the window, both land at (0, 0).
#[derive(Resource, Default, Clone, Copy, Debug)]
struct LastPointer {
    x: i32,
    y: i32,
    valid: bool,
    /// The view the last cursor position was converted against. Button and
    /// wheel events reuse it so they route to the same view as the move.
    target: Option<Entity>,
}

#[allow(clippy::needless_pass_by_value)]
fn forward_cursor_moved(
    mut reader: MessageReader<CursorMoved>,
    mut queue: ResMut<NoesisInputQueue>,
    mut last: ResMut<LastPointer>,
    window: Single<(Entity, &Window), With<PrimaryWindow>>,
    views: Query<(Entity, &NoesisView)>,
) {
    let (primary_window, window) = (window.0, window.1);
    // Lowest `Entity`, not query order: must match the render side's primary view.
    let Some((entity, scene)) = views.iter().min_by_key(|(entity, _)| *entity) else {
        reader.read();
        return;
    };
    for ev in reader.read() {
        if ev.window != primary_window {
            continue;
        }
        if let Some((x, y)) = to_view_coords(window, scene, ev.position.x, ev.position.y) {
            last.x = x;
            last.y = y;
            last.valid = true;
            last.target = Some(entity);
            queue.push_to(entity, NoesisInputEvent::MouseMove { x, y });
        }
    }
}

/// Moves the Noesis pointer off-view when the cursor leaves the primary window,
/// so hover highlights clear and [`NoesisPointerOverUi`] resets. `CursorLeft`
/// has no position, so the move goes to (-1, -1), which hit-tests nothing.
#[allow(clippy::needless_pass_by_value)]
fn forward_cursor_left(
    mut reader: MessageReader<CursorLeft>,
    mut queue: ResMut<NoesisInputQueue>,
    mut last: ResMut<LastPointer>,
    primary_window: Single<Entity, With<PrimaryWindow>>,
) {
    let primary_window = *primary_window;
    let mut left_primary = false;
    for ev in reader.read() {
        if ev.window == primary_window {
            left_primary = true;
        }
    }
    if left_primary {
        queue.push_to_opt(last.target, NoesisInputEvent::MouseMove { x: -1, y: -1 });
        last.valid = false;
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
        // Press must hit-test at the last move, whatever the message order.
        if last.valid {
            queue.push_to_opt(last.target, NoesisInputEvent::MouseMove { x, y });
        }
        queue.push_to_opt(
            last.target,
            NoesisInputEvent::MouseButton {
                down: matches!(ev.state, ButtonState::Pressed),
                x,
                y,
                button,
            },
        );
    }
}

#[allow(clippy::needless_pass_by_value)]
fn forward_mouse_wheel(
    mut reader: MessageReader<MouseWheel>,
    mut queue: ResMut<NoesisInputQueue>,
    last: Res<LastPointer>,
) {
    // Wheel only, never Scroll: ScrollViewers listen on both and would scroll twice.
    for ev in reader.read() {
        let (x, y) = if last.valid { (last.x, last.y) } else { (0, 0) };
        // Heuristic: 40 px per line.
        let lines_y = match ev.unit {
            MouseScrollUnit::Line => ev.y,
            MouseScrollUnit::Pixel => ev.y / 40.0,
        };
        let lines_x = match ev.unit {
            MouseScrollUnit::Line => ev.x,
            MouseScrollUnit::Pixel => ev.x / 40.0,
        };
        // Win32 `WHEEL_DELTA`: 120 per line.
        let wheel_delta = (lines_y * 120.0) as i32;
        let hwheel_delta = (lines_x * 120.0) as i32;
        if wheel_delta != 0 {
            queue.push_to_opt(
                last.target,
                NoesisInputEvent::MouseWheel {
                    x,
                    y,
                    delta: wheel_delta,
                },
            );
        }
        if hwheel_delta != 0 {
            queue.push_to_opt(
                last.target,
                NoesisInputEvent::MouseHWheel {
                    x,
                    y,
                    delta: hwheel_delta,
                },
            );
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn forward_keyboard(mut reader: MessageReader<KeyboardInput>, mut queue: ResMut<NoesisInputQueue>) {
    for ev in reader.read() {
        // Repeats are forwarded as KeyDown: `View::KeyDown` has no repeat timer,
        // so held arrows and Backspace would otherwise act once.
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
    window: Single<(Entity, &Window), With<PrimaryWindow>>,
    views: Query<(Entity, &NoesisView)>,
) {
    let (primary_window, window) = (window.0, window.1);
    let Some((entity, scene)) = views.iter().min_by_key(|(entity, _)| *entity) else {
        reader.read();
        return;
    };
    for ev in reader.read() {
        if ev.window != primary_window {
            continue;
        }
        let Some((x, y)) = to_view_coords(window, scene, ev.position.x, ev.position.y) else {
            continue;
        };
        let id = ev.id;
        match ev.phase {
            TouchPhase::Started => queue.push_to(entity, NoesisInputEvent::TouchDown { x, y, id }),
            TouchPhase::Moved => queue.push_to(entity, NoesisInputEvent::TouchMove { x, y, id }),
            TouchPhase::Ended | TouchPhase::Canceled => {
                queue.push_to(entity, NoesisInputEvent::TouchUp { x, y, id });
            }
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn forward_focus(
    mut reader: MessageReader<WindowFocused>,
    mut queue: ResMut<NoesisInputQueue>,
    primary_window: Single<Entity, With<PrimaryWindow>>,
) {
    let primary_window = *primary_window;
    for ev in reader.read() {
        if ev.window != primary_window {
            continue;
        }
        queue.push(NoesisInputEvent::Focus(ev.focused));
    }
}

/// Sets every [`NoesisView::size`] to the primary window's physical size, so
/// the composite blit is 1:1. `ensure_noesis_scene` resizes the `View` and its
/// intermediates in place later in the same frame.
#[allow(clippy::needless_pass_by_value)]
fn resize_noesis_scene(
    mut reader: MessageReader<WindowResized>,
    mut views: Query<&mut NoesisView>,
    window: Single<(Entity, &Window), With<PrimaryWindow>>,
) {
    let (primary_window, window) = (window.0, window.1);
    if views.is_empty() {
        reader.read();
        return;
    }
    for ev in reader.read() {
        if ev.window != primary_window {
            continue;
        }
        let physical = window.physical_size();
        if physical.x > 0 && physical.y > 0 {
            for mut scene in &mut views {
                scene.size = UVec2::new(physical.x, physical.y);
            }
        }
    }
}

/// Installs the input forwarders, [`NoesisInputQueue`] and
/// [`NoesisPointerOverUi`].
///
/// [`NoesisPlugin`](crate::NoesisPlugin) adds it through
/// [`NoesisPlugin::add_bridge_plugins`](crate::NoesisPlugin::add_bridge_plugins);
/// adding it again panics as a duplicate plugin.
pub struct NoesisInputPlugin;

impl Plugin for NoesisInputPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NoesisInputQueue>()
            .init_resource::<LastPointer>()
            .init_resource::<NoesisPointerOverUi>()
            .add_systems(
                PreUpdate,
                (
                    resize_noesis_scene,
                    forward_cursor_moved,
                    forward_cursor_left,
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
