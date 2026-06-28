# Phase 5 — Input + animation tick

Wire Bevy's input events and real-time clock into the Noesis `View` so XAML
controls light up (hover/press/click, focus, keyboard input, scroll, touch)
and animations play correctly (`Storyboard`, `VisualState`, `Time`-driven
bindings).

Deliverable: a generalized **viewer** example that accepts a single `.xaml`
file or a directory of them, lets us flip through scenes, exercises input,
and screenshots on demand. We use this tool to eval each sub-phase against a
growing corpus of targeted XAMLs (under `assets/phase5/`), then graduate to
driving the SDK's own `Data/*.xaml` samples.

## Scope

**In:**
- Pointer: `MouseMove`, `MouseButtonDown`/`Up`, `MouseDoubleClick`,
  `MouseWheel` + `Scroll`/`HScroll`.
- Keyboard: `KeyDown`/`KeyUp` (via a Bevy `KeyCode` → Noesis `Key` map), plus
  `Char` fed from Bevy text input events.
- Touch: `TouchDown`/`Move`/`Up` with Bevy's stable `Touch::id` as the Noesis
  touch id.
- Focus: `View::Activate()` / `Deactivate()` driven by Bevy `WindowFocused`.
- Time: `View::Update` fed from `Time<Real>::elapsed_secs_f64()` (already
  wired; formalize contract and document the clock source).
- Resize + DPI: recompute `View::SetSize` from `Window` physical size,
  rebuild the intermediate texture to match, translate cursor coords from
  Bevy logical → Noesis physical pixels using `Window::scale_factor()`.
- Viewer tool: `xaml_viewer` example replacing `hello_xaml` + merging
  `hello_xaml_screenshot`'s capture hooks. Accepts either a single XAML path
  or a directory; cycles with `[` / `]`; `P` toggles PPAA; `R` reloads;
  `S` triggers a screenshot of the current scene.

**Deferred:**
- **IME / text composition.** Bevy 0.18 exposes pre-edit via `Ime`; we do
  not parse composition strings or forward IME state. Plain ASCII / Unicode
  chars that arrive via `KeyboardInput::text` are fine.
- Gamepad / virtual-keyboard controls.
- Multi-view input routing (Phase 7).
- Windows-only focus/activate quirks (Phase 8).

## Architectural notes

- **Input crosses sub-apps.** Bevy events live on the main world; the `View`
  lives in the render world (render-thread-pinned). We batch events into a
  `NoesisInputQueue` resource on the main app, extract-clone it into the
  render app, and the render-thread `drive_noesis_frame` system drains the
  queue by calling `view.*` methods before `view.update(time)`. Input and
  time both feed the same frame — no split between "simulate" and "render".
- **Event ordering matters.** Noesis's `View::MouseButtonDown` requires a
  preceding `MouseMove` at the press coordinates (otherwise hit-test is
  against stale state). We enqueue `MouseMove` before every `Mouse*` or
  `Touch*` event that carries an `(x, y)`.
- **Coord conversion.** Bevy `CursorMoved::position` is logical px with the
  origin top-left (matching Noesis). We multiply by
  `Window::scale_factor()` to get physical px and pass `i32`s to Noesis.
- **Char event shape.** Noesis's `Char` takes a single `u32` codepoint. We
  iterate `KeyboardInput::text.as_deref().unwrap_or("")` chars and enqueue
  one `Char(cp)` per char, between the matching `KeyDown`/`KeyUp`.
- **Key map.** A `key_map.rs` module maps `KeyCode` → `Noesis::Key`. Not all
  keys round-trip — e.g. `KeyCode::Comma` has no direct Noesis peer and
  relies on `Char` for text entry. Unmapped keys return `Key_None`, which
  Noesis ignores.
- **Activate at startup.** Noesis ignores keyboard input until
  `View::Activate()` is called; we activate on first frame after the scene
  is built and on `WindowFocused { focused: true, .. }`.
- **Resize.** When `Window` physical size or scale factor changes, we call
  `view.set_size(w, h)` and recreate the intermediate `wgpu::Texture` at
  the new size (the blit already stretches, but sharp AA requires matching
  physical px). Gated behind a small "size changed" check to avoid
  recreating every frame.

## Sub-phases

Check each box when its visual/test passes.

### 5.A — FFI surface in `dm_noesis_runtime`

- [x] (prereq) `View::set_size`, `set_flags`, `update` already bound.
- [ ] **5.A.1 — Pointer methods.** Add `dm_noesis_view_mouse_{move,down,up,
      double_click,wheel}`, `..._scroll`, `..._h_scroll` to the C++ shim
      + Rust FFI extern + safe `View` methods. `MouseButton` mirrored as a
      Rust `#[repr(i32)]` enum matching `InputEnums.h`.
- [ ] **5.A.2 — Keyboard methods.** `dm_noesis_view_key_{down,up}` +
      `dm_noesis_view_char`. `Key` mirrored as a `#[repr(i32)]` enum.
      Rather than re-type every ~230 `Key_*` values, generate the enum
      from `InputEnums.h` with a small build-script parser or a one-shot
      pasted table — decide when writing.
- [ ] **5.A.3 — Touch methods.** `dm_noesis_view_touch_{down,move,up}`.
- [ ] **5.A.4 — Focus methods.** `dm_noesis_view_activate` /
      `deactivate`.
- [ ] **5.A.5 — Unit test.** Extend `dm_noesis_runtime/tests/headless_xaml.rs`
      (or add a sibling `headless_input.rs`) to load a `<Button>` XAML,
      feed synthetic mouse events, assert `View::Update` returns `true`
      (something changed) and — if feasible via a `Noesis::EventHandler`
      trampoline — that a `Click` actually fires. If the click-assertion
      needs more C++ plumbing than the phase budgets, downgrade to
      checking `update()` returned `true` at the expected frames.

### 5.B — `NoesisInputPlugin` (main app)

- [ ] **5.B.1 — `NoesisInputQueue` resource.** `Vec<NoesisInputEvent>`
      with a pointer / key / char / touch / focus / resize variant. Drains
      into the render world.
- [ ] **5.B.2 — Bevy event forwarders.** Read systems for
      `CursorMoved`, `MouseButtonInput`, `MouseWheel`, `KeyboardInput`,
      `TouchInput`, `WindowFocused`, `WindowResized`. Convert each to the
      queue variant, applying the scale-factor conversion here (main app
      owns the `Window`). Runs in `PreUpdate`.
- [ ] **5.B.3 — Key map.** `key_map::from_bevy(KeyCode) -> NoesisKey`.
      Aim for parity with Bevy keys that have obvious analogs; fall back
      to `Key_None`. Add a small unit test pinning a dozen round-trips
      (letters, arrows, modifiers, F-keys).
- [ ] **5.B.4 — Extract.** `ExtractResourcePlugin<NoesisInputQueue>` with
      a `ClearEvents`-style drain on extract, so the render world gets a
      fresh snapshot each frame and the main world clears afterward.

### 5.C — Render-world ingestion

- [ ] **5.C.1 — Drain queue.** New `apply_noesis_input` system running
      in the `Render` schedule between `ensure_noesis_scene` and
      `drive_noesis_frame`. Walks `NoesisInputQueue`, calls matching
      `View::*` methods, handles `Resize` by updating `NoesisScene.size`
      (in-place on the extracted resource is fine — it's a clone) and
      setting a `resized` flag consumed by `ensure_scene` to rebuild
      the intermediate.
- [ ] **5.C.2 — Activate on scene build.** In `ensure_scene`, call
      `view.activate()` once after creation. Also handle subsequent
      `Focus(true/false)` events.
- [ ] **5.C.3 — Time clock review.** Lock in `Time<Real>` as the clock
      and document that XAML `Storyboard`s play in wall-clock seconds
      regardless of Bevy's fixed-update state.

### 5.D — `xaml_viewer` example (supersedes `hello_xaml*`)

Replaces `hello_xaml` + absorbs `hello_xaml_screenshot`'s capture paths.

- [ ] **5.D.1 — CLI / env config.** Arg parsing via a tiny hand-rolled
      parser (no new dep) or `std::env::args` directly. Accepted forms:
      - `xaml_viewer`                        — default dir `assets/phase5`
      - `xaml_viewer path/to/file.xaml`      — single scene
      - `xaml_viewer path/to/dir`            — cycle through `*.xaml`
      - Env passthrough for CI / screenshot runs:
        `NOESIS_VIEWER_PATH`, `NOESIS_SCREENSHOT`,
        `NOESIS_SCREENSHOT_FRAMES`, `NOESIS_VIEWER_EXIT_AFTER=1`.
- [ ] **5.D.2 — Scene cycling.** `[` / `]` advance the index; `Home` /
      `End` jump. Reload with `R`. Toggle `PPAA` with `P`. Trigger a
      screenshot with `S` (written as `<stem>.png` next to the XAML, or
      `$NOESIS_SCREENSHOT` when set).
- [ ] **5.D.3 — Interactive input smoke.** With input plumbing from 5.B
      /5.C live, hover + clicking a `<Button>` in one of the test scenes
      visibly changes its brush/content (built-in WPF `IsMouseOver`
      triggers suffice — no code-behind required in XAML).
- [ ] **5.D.4 — Headless mode.** When `NOESIS_VIEWER_EXIT_AFTER` is set,
      run N frames, screenshot, exit — mirrors the current
      `hello_xaml_screenshot` for CI / Claude-eval runs.
- [ ] **5.D.5 — Retire old examples.** Delete `hello_xaml.rs`,
      `hello_xaml_screenshot.rs`, `hello_text.rs` once `xaml_viewer`
      covers all three. Update `CLAUDE.md` command list.

### 5.E — Test XAML corpus (`assets/phase5/`)

Hand-authored, targeted, small. Each one exercises a specific bit of
Phase 5 plumbing we can eyeball in the viewer and screenshot.

- [ ] `01_button_hover.xaml`    — `<Button>` whose Background switches
      on `IsMouseOver` (pointer move + hit-test).
- [ ] `02_button_click.xaml`    — `<Button>` with a `<Storyboard>` on
      `Click` that rotates or translates a child (pointer down/up +
      animation tick).
- [ ] `03_scroll.xaml`           — `<ScrollViewer>` wrapping a tall
      `StackPanel` (wheel + scroll routing).
- [ ] `04_textbox.xaml`          — `<TextBox>` (focus, keyboard, char,
      caret blink via storyboard — tests activate + key events + time).
- [ ] `05_resize.xaml`           — `<Grid>` with percentage rows +
      corner anchors; resizing the window should re-layout.
- [ ] `06_storyboard_idle.xaml`  — pure time-driven animation, no
      input; screenshot comparison at frame 0/30/60.
- [ ] `07_touch.xaml`            — visual hit markers on touch events
      (skipped on desktop unless we simulate via `Touch` inputs).

Each file is a few dozen lines of XAML; keep them focused. Favor built-in
controls + triggers over code-behind so Noesis parses them as-is.

### 5.F — Graduation to SDK samples

Once our corpus passes, point the viewer at `$NOESIS_SDK_DIR/Data/`
(symlink `assets/Data -> $NOESIS_SDK_DIR/Data`, already used in spirit
for `Noesis.xaml`). Expect bugs from shader variants that 4.E deferred;
file each one as a follow-up on the 4.E checklist rather than stuffing
into Phase 5.

- [ ] `Noesis.xaml`              — primary smoke scene (already working,
      re-verify under interactive input).
- [ ] `Styles.xaml`              — large control gallery.
- [ ] `CarHud.xaml`              — pattern + linear-gradient heavy.
- [ ] `Text.xaml`                — once 4.F.3 lands (SDF), verify font
      rendering at multiple sizes.
- [ ] `Transform3D.xaml`         — depth / perspective transforms.
- [ ] `Effects.xaml`             — likely hits Phase 6 shader variants;
      expected to fail, file gaps.
- [ ] `Lottie.xaml`              — animation stress; expected to reveal
      any time-tick drift.

## Open questions

- **How authoritative is the Bevy→Noesis key map?** WPF keys diverge from
  ANSI/USB-HID layouts in a few subtle places (e.g. `OEM_1..8`). For
  Phase 5 we map the obvious set and treat anything else as Char-only.
- **Should resize rebuild the intermediate or rescale the blit?** We
  rebuild — simpler, same cost as the initial build. If it ever shows up
  in a profile, we switch to blit-scale + a viewport parameter.
- **`Char` for modifier combos?** WPF semantics say `Char` is the
  text-input codepoint, already produced by the OS-level layout. Bevy
  0.18 provides that string via `KeyboardInput::text` — trust it.

## Commands

- `cargo check -p dm_noesis_bevy`
- `cargo test -p dm_noesis_runtime -p dm_noesis_bevy`
- `cargo run -p dm_noesis_bevy --example xaml_viewer` — dir mode
- `cargo run -p dm_noesis_bevy --example xaml_viewer assets/phase5/01_button_hover.xaml`
- `NOESIS_VIEWER_EXIT_AFTER=1 NOESIS_SCREENSHOT=out.png \
   cargo run -p dm_noesis_bevy --example xaml_viewer <file>` — headless.
