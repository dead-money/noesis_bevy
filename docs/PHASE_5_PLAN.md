# Phase 5 — Input + Animation Tick

## Goal

Make the Noesis View *interactive* and *alive*. Two slices that fit one phase:

1. **Animation tick.** Drive `View::Update(time)` from Bevy `Time<Real>` so XAML
   `Storyboard`s, `EventTrigger`s, and any property animation actually progress.
   Today the test rig calls `view.update(0.0)` on every frame, which freezes
   anything that isn't a static layout.
2. **Input.** Forward Bevy's mouse, keyboard, touch, and focus events to the
   View so XAML `Button.Click`, `TextBox`, `IsMouseOver` styles, scrollbars,
   and any custom `Behavior` start responding.

Both slices touch the same `NoesisRenderState` and the same render-app
schedule — bundling them avoids two passes through the FFI surface.

## Success criteria

- `cargo run -p dm_noesis_bevy --example hello_button` opens a window with
  a XAML `Button` whose hover styling visibly changes on mouse-over and
  whose `Click` handler updates a `TextBlock` (`Foreground` brush swap is
  enough — no command binding plumbing needed for this phase).
- `cargo run -p dm_noesis_bevy --example hello_animation` runs a XAML
  `Storyboard` (`DoubleAnimation` on `Opacity` or `RenderTransform.Rotation`)
  and the animation visibly progresses; `Time::elapsed_secs_f64()` advances
  the `View::Update` clock.
- New headless test `tests/headless_input.rs` registers a `WgpuRenderDevice`,
  loads a `Button` XAML with a known `IsMouseOver` style, drives a synthetic
  `MouseMove`/`MouseButtonDown`/`MouseButtonUp` sequence, and reads back
  pixels at the button's known position. Asserts hover and pressed colors
  differ from the default.
- All existing tests still pass; clippy stays clean.

## Surface to wire

### From Noesis (`Include/NsGui/IView.h` + `Include/NsGui/InputEnums.h`)

```cpp
virtual bool MouseMove(int x, int y);
virtual bool MouseButtonDown(int x, int y, MouseButton button);
virtual bool MouseButtonUp(int x, int y, MouseButton button);
virtual bool MouseDoubleClick(int x, int y, MouseButton button);
virtual bool MouseWheel(int x, int y, int wheelRotation);
virtual bool MouseHWheel(int x, int y, int wheelRotation);

virtual bool TouchDown(int x, int y, uint64_t id);
virtual bool TouchMove(int x, int y, uint64_t id);
virtual bool TouchUp  (int x, int y, uint64_t id);

virtual bool KeyDown(Key key);
virtual bool KeyUp  (Key key);
virtual bool Char(uint32_t ch);

virtual void Activate();
virtual void Deactivate();

virtual bool Update(double timeInSeconds);
```

`MouseButton` is `Left, Right, Middle, XButton1, XButton2`. `Key` is the long
WPF-style enum in `InputEnums.h` (`Key_A`..`Key_Z`, `Key_F1`..`Key_F24`,
`Key_NumPad0`..`Key_NumPad9`, `Key_LeftCtrl`, `Key_LeftShift`, etc.).
`Update` returns `true` when something changed (animation progressed, layout
invalidated); paired with `Renderer::UpdateRenderTree` already in the loop.

### From Bevy (0.18)

- `Time<Real>` — wall-clock since startup. `elapsed_secs_f64()` is the value
  to feed `View::Update`. (Fixed-update is irrelevant here — Noesis runs once
  per render frame.)
- `MessageReader<MouseButtonInput>` (bevy_input) — `state` (Pressed/Released)
  + `button: bevy::input::mouse::MouseButton`.
- `MessageReader<MouseWheel>` — `unit: MouseScrollUnit`, `x`, `y`.
- `MessageReader<CursorMoved>` (bevy_window) — `position: Vec2` in window
  coords; the Noesis View takes `int x, int y` in the same coord space the
  XAML was laid out against (i.e. the `NoesisScene::size` rectangle).
- `MessageReader<KeyboardInput>` (bevy_input) — `logical_key`, `state`.
  Char input also surfaces here as `logical_key: Key::Character(SmolStr)`.
- `Touches` (bevy_input) — `iter()` over active touches per frame.
- `WindowFocused` events — drive `Activate` / `Deactivate`.

## FFI design (`dm_noesis_runtime/`)

### C ABI additions (`cpp/noesis_view.{h,cpp}`)

One thin trampoline per IView method, no enum mirroring on the C side
beyond `int` for button / key constants:

```c
// Time
void dm_noesis_view_update(void* view, double time_seconds);  // already exists

// Pointer
bool dm_noesis_view_mouse_move(void* view, int x, int y);
bool dm_noesis_view_mouse_button_down(void* view, int x, int y, int button);
bool dm_noesis_view_mouse_button_up  (void* view, int x, int y, int button);
bool dm_noesis_view_mouse_double_click(void* view, int x, int y, int button);
bool dm_noesis_view_mouse_wheel  (void* view, int x, int y, int rotation);
bool dm_noesis_view_mouse_hwheel (void* view, int x, int y, int rotation);

// Touch
bool dm_noesis_view_touch_down(void* view, int x, int y, uint64_t id);
bool dm_noesis_view_touch_move(void* view, int x, int y, uint64_t id);
bool dm_noesis_view_touch_up  (void* view, int x, int y, uint64_t id);

// Keyboard
bool dm_noesis_view_key_down(void* view, int key);
bool dm_noesis_view_key_up  (void* view, int key);
bool dm_noesis_view_char    (void* view, uint32_t codepoint);

// Focus
void dm_noesis_view_activate  (void* view);
void dm_noesis_view_deactivate(void* view);
```

Inside each, cast `view` to `Noesis::IView*`, cast `int button` to
`Noesis::MouseButton`, `int key` to `Noesis::Key`, call. Return the bool the
View hands back (true = handled / state changed).

### Rust safe wrappers (`dm_noesis_runtime/src/view.rs`)

Mirror enums:

```rust
#[repr(i32)]
pub enum MouseButton {
    Left = 0, Right = 1, Middle = 2, XButton1 = 3, XButton2 = 4,
}

/// Subset of the WPF Key enum we're translating into. Comprehensive enough
/// to cover the keys Bevy surfaces directly in `KeyCode`; obscure keys
/// (KanaMode, HanjaMode, ...) can be added on demand.
#[repr(i32)]
pub enum Key { /* generated to match InputEnums.h Key_* indices */ }
```

Methods on `View`:

```rust
impl View {
    pub fn mouse_move(&mut self, x: i32, y: i32) -> bool;
    pub fn mouse_button_down(&mut self, x: i32, y: i32, button: MouseButton) -> bool;
    pub fn mouse_button_up  (&mut self, x: i32, y: i32, button: MouseButton) -> bool;
    pub fn mouse_double_click(&mut self, x: i32, y: i32, button: MouseButton) -> bool;
    pub fn mouse_wheel  (&mut self, x: i32, y: i32, rotation: i32) -> bool;
    pub fn mouse_hwheel (&mut self, x: i32, y: i32, rotation: i32) -> bool;
    pub fn touch_down(&mut self, x: i32, y: i32, id: u64) -> bool;
    pub fn touch_move(&mut self, x: i32, y: i32, id: u64) -> bool;
    pub fn touch_up  (&mut self, x: i32, y: i32, id: u64) -> bool;
    pub fn key_down(&mut self, key: Key) -> bool;
    pub fn key_up  (&mut self, key: Key) -> bool;
    pub fn char_input(&mut self, codepoint: u32) -> bool;
    pub fn activate(&mut self);
    pub fn deactivate(&mut self);
}
```

`update` already exists; nothing to change there.

## Bevy plugin design (`dm_noesis_bevy/src/`)

### New module `src/input.rs`

Two responsibilities:

1. **Collect** Bevy main-world input events into a small `NoesisInputFrame`
   value type each frame.
2. **Extract** that frame to the render world via `ExtractResource` (same
   pattern `XamlRegistry` already uses), so the render-app system can call
   View methods without crossing thread boundaries.

```rust
#[derive(Resource, ExtractResource, Clone, Default, Debug)]
pub struct NoesisInputFrame {
    pub cursor: Option<Vec2>,           // window-space, latest CursorMoved
    pub buttons: Vec<MouseButtonEvent>, // press/release this frame
    pub wheel: Vec<MouseWheelEvent>,
    pub keys: Vec<KeyEvent>,
    pub chars: Vec<u32>,
    pub touches: Vec<TouchEvent>,
    pub focus: Option<bool>,            // Some(true)=Activate, Some(false)=Deactivate
    pub time_seconds: f64,              // Time<Real>::elapsed_secs_f64()
}
```

Main-world system `collect_noesis_input`:
- Reads the Bevy event readers each frame.
- Maps Bevy key codes → Noesis `Key` values via a const lookup table.
- Stores into the `NoesisInputFrame` resource. Cleared at the top of each
  frame so `ExtractResource` ships only this frame's events.

### Render-world dispatch (`src/render.rs`)

Where the existing render-schedule system already drives Noesis (`View::Update`
→ `Renderer::UpdateRenderTree` → render), inject input dispatch *before*
`Update`:

```text
extract_noesis_input            // ExtractResource clones the frame in
dispatch_noesis_input           // calls View::mouse_move, key_down, etc.
view.update(time_seconds)       // existing — now reads the extracted time
renderer.update_render_tree()
renderer.render_offscreen()
renderer.render(...)
```

`dispatch_noesis_input` walks the frame in the same order Bevy produced
events: cursor → buttons → wheel → keys → chars → touches → focus. Coordinate
transform from window-space `Vec2` to View-space `(i32, i32)`:
multiply by `NoesisScene::size / window_size`, round to nearest integer.
For Phase 5 we assume window size == `NoesisScene::size`; the proper resize
story lands with Phase 7 (multi-view + window resize).

### Time source

Replace the literal `0.0` in the existing `view.update(0.0)` call with
`input_frame.time_seconds` extracted from `Time<Real>`. That's the smallest
possible diff. `Time<Virtual>` would be better long-term (lets users pause
animations) but it's an add-on, not the critical path.

## Sub-phase sequencing

Each row is a single PR-sized commit. Order chosen so each step compiles
and is verifiable on its own.

- **5.0 — Animation tick only.** Wire `Time<Real>` → render-world resource →
  `View::Update`. No input yet. Verification: a one-line XAML
  `<TextBlock Opacity="0.0" Text="..."/>` driven by a `Storyboard` that
  fades from 0→1; `examples/hello_animation` shows it visibly fade in.
  Smallest possible diff to confirm the time path works end to end.
- **5.1 — FFI surface for input.** `dm_noesis_runtime/cpp/noesis_view.cpp` C
  trampolines + `dm_noesis_runtime/src/view.rs` safe wrappers + the `MouseButton`
  / `Key` enum mirrors. Unit tests in `dm_noesis_runtime/tests/` exercise each
  method against a no-op View (no rendering needed).
- **5.2 — Bevy input collection.** `src/input.rs` with `NoesisInputFrame`,
  `collect_noesis_input` system, key/button mapping table. No render-side
  consumption yet. Smoke-test by `info!`-logging the frame each tick
  and waving the mouse around.
- **5.3 — Render-world dispatch.** `dispatch_noesis_input` system; coord
  remap. Bring up `examples/hello_button` (Button + IsMouseOver style +
  Click → TextBlock color swap). Visual verification.
- **5.4 — Headless input regression.** `tests/headless_input.rs` — a
  `RecordingDevice`-style harness that drives synthetic mouse events and
  asserts the readback shows the button's hover color at the button's
  pixel position. Locks in the dispatch order.
- **5.5 — Documentation + cleanup.** Update CLAUDE.md phase tracker,
  README roadmap, drop the placeholder for any unsupported keys.

## Verification scenes

Land alongside the implementation in `assets/`:

- `assets/hello_animation.xaml` — Grid background + center TextBlock with
  a `Storyboard` on `Opacity` (0→1 over 2s, repeat forever). Verifies
  the time-driving system end to end. **No input required.**
- `assets/hello_button.xaml` — single centered `Button` with a `Style` on
  `IsMouseOver` (background swap) and `IsPressed` (background swap), plus
  a `Click` handler that toggles a `TextBlock.Foreground` between two
  named colors via a tiny `EventTrigger` storyboard. Verifies pointer
  routing, hit testing, and that `View::MouseButton{Down,Up}` works.

## Open questions to resolve in flight

- **`MouseDoubleClick` in Bevy.** Bevy doesn't natively report double-click;
  Noesis can synthesize it from rapid `MouseButtonDown` pairs but exposes
  the explicit method too. Options: (a) skip — let Noesis's own
  double-click threshold path fire from our regular Down/Up sequence; or
  (b) detect ourselves with `View::SetDoubleTapTimeThreshold`. Pick (a)
  unless empirical testing shows Noesis isn't synthesizing.
- **Coordinate mapping vs. `NoesisScene::size`.** The current intermediate
  texture is sized to `NoesisScene::size`, then blitted to the camera's
  `ViewTarget`. If those sizes differ from the window, hit testing needs
  to scale cursor coords through the same transform. Phase 5 assumes
  `NoesisScene::size == window_size`; the resize story is Phase 7.
- **Bevy `KeyCode` → Noesis `Key`.** Most map 1:1 (alphanumerics, F-keys,
  Ctrl/Shift). IME / kanji / hangul keys are uncommon for game UIs;
  surface them as `Key_None` for now and revisit if a real scene needs
  them.
- **Touch + mouse together.** Noesis has `SetEmulateTouch` for one-or-the-
  other dispatch. Default off (handle both); revisit if synthesized
  events double-fire on touchscreens.
- **Render-thread re-entrancy.** Noesis input methods may invoke XAML
  event handlers synchronously (Click handlers, etc.). Those run on the
  render thread in our setup. Document this restriction; users who need
  to do heavy main-app work from a Click should bridge via a Bevy
  `Message`/event channel rather than blocking inside the handler.
- **Keyboard focus ownership.** Bevy doesn't have a notion of "the Noesis
  view has focus"; if the app has multiple input consumers (e.g. a 3D
  camera + Noesis UI), the UI should claim focus when a Button is
  hit. The `View::*` methods return `bool` (true = handled) — Phase 5
  threads that return value back into a Bevy resource so other systems
  can early-out. Concrete API for that lands with Phase 7's multi-view
  story; for Phase 5 we just store the latest `handled` flag and let
  callers read it if they want.
