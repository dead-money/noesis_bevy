//! Bevy → Noesis input forwarding (Phase 5.B).
//!
//! The main app observes Bevy's raw input events and a few window events,
//! converts each into a [`NoesisInputEvent`], and pushes it onto a shared
//! [`NoesisInputQueue`] resource. The queue is cloned into the render world
//! via [`ExtractResource`] each frame; the render-side `apply_noesis_input`
//! system (defined in `render.rs`) drains it onto the live
//! [`dm_noesis_runtime::view::View`] just before the frame is driven.
//!
//! # Coordinate handling
//!
//! Bevy delivers cursor positions in *logical* pixels relative to the
//! window. Noesis hit-tests in the view's own pixel space — which is
//! whatever [`NoesisScene::size`] is set to (the intermediate texture).
//! We convert on the main side, collapsing Window scale factor and any
//! intermediate-vs-window size mismatch into a single ratio:
//!
//! ```text
//!   view_x = cursor_logical_x * view_w / window_logical_w
//!   view_y = cursor_logical_y * view_h / window_logical_h
//! ```
//!
//! When the intermediate is made to match the window physical size (a
//! straight-line Phase 5.C follow-up once we wire `WindowResized`), this
//! ratio reduces to the scale factor.
//!
//! # Queue lifecycle
//!
//! Systems that push run in `PreUpdate`. The resource is cloned to the
//! render world by [`ExtractResourcePlugin`] between the main schedule
//! and the render sub-app's own schedules. Since Bevy's `Last` runs
//! *before* the render sub-app's `ExtractSchedule` (the main schedule
//! completes, then sub-apps run), we can't clear in `Last` — that would
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
//!   Frame N+1 PreUpdate: clear (drops A, B, C from main queue) — then push again
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
use dm_noesis_runtime::view::{Key, MouseButton};

use crate::render::NoesisScene;

// ── key map ────────────────────────────────────────────────────────────────

pub mod key_map;

// ── Events and queue ───────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub enum NoesisInputEvent {
    MouseMove {
        x: i32,
        y: i32,
    },
    MouseButton {
        down: bool,
        x: i32,
        y: i32,
        button: MouseButton,
    },
    MouseWheel {
        x: i32,
        y: i32,
        delta: i32,
    },
    Scroll {
        x: i32,
        y: i32,
        value: f32,
        horizontal: bool,
    },
    TouchDown {
        x: i32,
        y: i32,
        id: u64,
    },
    TouchMove {
        x: i32,
        y: i32,
        id: u64,
    },
    TouchUp {
        x: i32,
        y: i32,
        id: u64,
    },
    KeyDown(Key),
    KeyUp(Key),
    Char(u32),
    Focus(bool),
}

/// Batched input events waiting to be drained onto the Noesis [`View`].
/// Populated by systems in this module; drained by the render-world
/// `apply_noesis_input` system every frame.
#[derive(Resource, ExtractResource, Clone, Default, Debug)]
pub struct NoesisInputQueue {
    pub events: Vec<NoesisInputEvent>,
}

impl NoesisInputQueue {
    pub fn push(&mut self, ev: NoesisInputEvent) {
        self.events.push(ev);
    }

    pub fn drain(&mut self) -> std::vec::Drain<'_, NoesisInputEvent> {
        self.events.drain(..)
    }
}

// ── Coordinate conversion ──────────────────────────────────────────────────

/// Scale a logical-px point on the window into Noesis view-pixel space.
/// Returns `None` when the window has zero size (startup race).
fn to_view_coords(window: &Window, scene: &NoesisScene, x: f32, y: f32) -> Option<(i32, i32)> {
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
/// a move-at-press-coord to every MouseButton / Touch event. Noesis
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
    scene: Option<Res<NoesisScene>>,
) {
    let Some(scene) = scene else {
        reader.read(); // drop events so we don't replay them later
        return;
    };
    for ev in reader.read() {
        if let Some((x, y)) = to_view_coords(&window, &scene, ev.position.x, ev.position.y) {
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
        // Noesis expects the press coord to match the last MouseMove. We
        // re-enqueue the last known position to stay consistent in the
        // unlikely case events arrive in a surprising order.
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
    // We forward MouseWheel as a Noesis `MouseWheel` event (Windows-style,
    // 120 units per detent) AND as a `Scroll` event (line count) so controls
    // that listen to either get the right signal. Noesis only handles the
    // first one it cares about; redundant calls are cheap.
    for ev in reader.read() {
        let (x, y) = if last.valid { (last.x, last.y) } else { (0, 0) };
        // Convert pixel scroll to "lines" for the Scroll path — rough
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
        // Skip synthetic repeat events — Noesis handles its own repeat
        // timing off the logical KeyDown/KeyUp pair.
        if ev.repeat {
            // But we DO still want the Char(s) on repeat so TextBox
            // auto-repeat works.
            if matches!(ev.state, ButtonState::Pressed) {
                if let Some(text) = ev.text.as_deref() {
                    for ch in text.chars() {
                        queue.push(NoesisInputEvent::Char(ch as u32));
                    }
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
    scene: Option<Res<NoesisScene>>,
) {
    let Some(scene) = scene else {
        reader.read();
        return;
    };
    for ev in reader.read() {
        let Some((x, y)) = to_view_coords(&window, &scene, ev.position.x, ev.position.y) else {
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

/// Snap `NoesisScene.size` to the window's physical pixel size on resize.
/// Makes the `NoesisNode` blit effectively 1:1 and brings the
/// cursor-coord ratio in `to_view_coords` down to just the scale factor.
///
/// Runs on the main app — the render-world clone of `NoesisScene` is
/// overwritten each frame via [`ExtractResource`], so the source of truth
/// has to live here. The render side picks up the new size on the next
/// frame's `ensure_scene`, which detects the mismatch and rebuilds the
/// intermediate texture + re-calls `View::set_size`.
#[allow(clippy::needless_pass_by_value)]
fn resize_noesis_scene(
    mut reader: MessageReader<WindowResized>,
    mut scene: Option<ResMut<NoesisScene>>,
    window: Single<&Window, With<PrimaryWindow>>,
) {
    let Some(scene) = scene.as_mut() else {
        reader.read();
        return;
    };
    for _ev in reader.read() {
        let physical = window.physical_size();
        if physical.x > 0 && physical.y > 0 {
            scene.size = UVec2::new(physical.x, physical.y);
        }
    }
}

/// Clear the main-app queue at the start of `PreUpdate`, before the
/// forwarders push new events. By the time this fires, the render
/// sub-app's extract has already copied whatever the previous frame
/// queued — see the module-level queue-lifecycle diagram.
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
                    // MUST run first — resets the queue so this frame's
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
