# dm_noesis_bevy

Bevy plugin that drives the closed-source Noesis GUI Native SDK and renders its UIs into a Bevy frame via a wgpu-backed `Noesis::RenderDevice`. Two sibling crates plus the SDK:

- **This crate** (`dm_noesis_bevy/`) — Bevy integration. Plugin, asset loaders, wgpu render-device impl on the Rust side of the FFI vtable, composite into Bevy via a render-graph blit pass. **`unsafe_code = forbid`.**
- **Sibling crate** [`../dm_noesis_runtime/`](https://github.com/dead-money/dm_noesis_runtime) — narrow C++ shim + Rust FFI to libNoesis. Owns the `RustRenderDevice : public Noesis::RenderDevice` C++ subclass that trampolines virtuals into a Rust vtable. `unsafe` allowed there. **We own and maintain this crate too** — it lives at `../dm_noesis_runtime` and is freely editable. Most new Bevy features start here: expand the C++ shim + Rust FFI to expose a Noesis capability, then build the Bevy-facing glue on top of it in this crate. When a feature needs something libNoesis offers but the shim doesn't yet wrap, the first step is to add it to `dm_noesis_runtime`, not to work around its absence.
- **Noesis Native SDK** (`$NOESIS_SDK_DIR`, e.g. `~/sdk/noesis-3.2.12`) — extracted from the developer's per-license SDK download. **NEVER commit** any SDK content; it is closed-source and per-developer-licensed.

## Attribution

Do not add `Co-Authored-By: Claude` trailers to commits or "Generated with Claude Code" footers to PR bodi
es. Author lines and PR bodies stay clean. `scripts/git-hooks/commit-msg` strips the
se defensively; activate per clone with `git config core.hooksPath scripts/git-hooks`. Do not work around
the hook.

## Reference material

- `RenderDevice` contract: `$NOESIS_SDK_DIR/Include/NsRender/{RenderDevice,Texture,RenderTarget}.h`. The frame protocol is documented in the header — read it first.
- Reference impl to mirror: `$NOESIS_SDK_DIR/Src/Packages/Render/GLRenderDevice/`. Our WGSL shaders are ports of `Shader.140.{vert,frag}`.
- Integration reference (no `SetProjectionMatrix`, frame-driving order): `$NOESIS_SDK_DIR/Src/Packages/App/IntegrationGLUT/Src/Main.cpp`.
- HTML reference: `$NOESIS_SDK_DIR/Doc/`.
- Sample XAML for visual tests: `$NOESIS_SDK_DIR/Data/` (symlinked to `assets/Data/`), e.g. `{Noesis,CarHud,Text,Lottie,Styles}.xaml`.

## Architectural invariants

Deviating from these is a design change — raise it before implementing.

- **NOT a port of Noesis.** We FFI to `libNoesis.so` and only author (a) the wgpu render device and (b) the Bevy glue. The XAML parser, layout engine, control library, animation system, and data binding all live inside libNoesis.
- **Two-crate split.** `dm_noesis_runtime` is the FFI quarantine (`unsafe` allowed). `dm_noesis_bevy` is `unsafe_code = forbid`.
- **Hand-written C ABI shim.** `dm_noesis_runtime/cpp/noesis_shim.{h,cpp}` is the only thing Rust binds against. **Do NOT bindgen** `NsCore`/`NsGui` — templates + `Ptr<T>` + virtual dispatch don't translate cleanly.
- **C++ subclass + Rust vtable is the FFI pattern.** Pure-virtual Noesis interfaces can't be implemented from Rust directly. The shim defines a C++ subclass (`RustRenderDevice`, `RustXamlProvider`, `RustFontProvider`, `RustTextureProvider`) whose every virtual trampolines into a Rust vtable struct + `void* userdata`. New provider/device hooks follow this same pattern; each exposes a safe `set_*`/`Registered` wrapper with a `Registered::provider_mut::<P>()` / `device_mut::<D>()` `TypeId`-checked downcast accessor.
- **Render-graph integration.** Each scene owns a graph-node-owned intermediate wgpu texture (`Rgba8Unorm`) that Noesis paints into via our `WgpuRenderDevice`; a fullscreen-triangle blit pass in the same node (`NoesisNode`, between `MainTransparentPass` and `EndMainPass`) copies that intermediate into the camera's `ViewTarget::main_texture_view`. Collapsing the intermediate and targeting `ViewTarget` directly is a known future perf win (see Open work).
- **Render-app thread ownership.** `View::Update(time)` and `Renderer::Render()` are driven from the render-app thread (stable whether pipelined rendering is on or off). `View`/`Renderer` wrappers are `!Send` and live as non-send resources (`NoesisRenderState`) in the render-app world. Global `dm_noesis_runtime::init()` / `shutdown()` stay on the main thread via `NoesisShutdownGuard`; render-app `Registered` guards must drop before that guard. `NoesisRenderState`'s `Drop` enforces `Renderer::shutdown` → `View` drop → device guard → provider guard, and must run on the render thread.
- **Clock source.** `NoesisRenderState` holds its own `std::time::Instant` origin and feeds `Instant::elapsed().as_secs_f64()` into `View::Update(time)`. Do **not** use `bevy::time::Time<Real>` — Bevy 0.18 does not extract it to the render world (reads `0.0` forever → storyboards/animations never advance).
- **Linear/sRGB and clip-space conventions** must match Bevy/wgpu. `DeviceCaps::linearRendering = false` (Noesis writes sRGB bytes directly into the `Rgba8Unorm` intermediate); the blit samples through an `Rgba8UnormSrgb` alias view when `ViewTarget` is sRGB to round-trip stored bytes exactly (requires the sRGB format in the texture's `view_formats`). Set `clipSpaceYInverted` / `depthRangeZeroToOne` from `DeviceCaps`; do **not** call `View::SetProjectionMatrix` (supplying a GL-style ortho makes Noesis's visibility pass cull child elements). WGSL projection is `v * M` to match Noesis's row-major `Matrix4` against WGSL column-major `mat4x4<f32>`.
- **SDK never in repo.** `NOESIS_SDK_DIR` env var; `build.rs` panics with a clear message when unset.

## How it fits together

- **Render device** (`src/render_device/wgpu_device.rs`) — `WgpuRenderDevice` implements the Rust side of the FFI vtable. `PipelineCache` keyed on `(shader, render_state, vertex_format)` with lazy build; vertex-layout dispatch from the SDK tables; a tiny `#ifdef` WGSL preprocessor (`shader_preproc.rs`) specializes the unified `noesis.wgsl` template. `UniformRing` + dynamic-offset bind groups stream per-batch uniforms. Real GPU textures / render targets via `HashMap<…Handle, Gpu…>`; group(2) holds the paint texture (`pattern` / `ramps` / `glyphs`) + sampler, gated by `HAS_PAINT_TEXTURE`-style defines.
- **Shaders implemented:** `Path_Solid`, `Path_AA_Solid`, `Mask`, `RGBA`, `Clear`; `PATH_PATTERN` (+AA) plus the CLAMP/REPEAT/MIRROR_{U,V}/MIRROR wrap variants (need `rect`+`tile` attrs); `PATH_LINEAR` / `PATH_RADIAL` (+AA, share the `ramps` texture); `SDF_SOLID` (glyphs). The `BlendMode` → `wgpu::BlendState` matrix covers Src / SrcOver / SrcOverMultiply / SrcOverScreen / SrcOverAdditive.
- **Providers** (`dm_noesis_runtime` shim + Bevy bridge) — XAML (`BevyXamlProvider` + `XamlRegistry`), fonts (`BevyFontProvider` + `FontRegistry`, `.ttf`/`.otf`/`.ttc`), textures/images (`BevyTextureProvider` + `ImageRegistry`, PNG/JPEG). Each registry is a `#[derive(ExtractResource)]` resource (Arc-level clones) extracted main→render; the render-world side holds a `Mutex`-guarded shared map that never crosses the main↔render boundary. `LoadTexture` hands decoded RGBA8 back to the C++ side, which calls the same `Noesis::RenderDevice` so images become real wgpu textures plugged into `Batch.pattern`.
- **Scene lifecycle** (`src/render.rs`) — `NoesisScene` describes a scene (URI, size, flags, theme, font-wait gates). `ensure_noesis_scene` builds/resizes the `View` (resize-in-place reuses the View, just `set_size` + rebuilds the intermediate); `drive_noesis_frame` runs `Update` → `UpdateRenderTree` → `RenderOffscreen` → `Render`. Scenes auto-`activate()` on build. `wait_for_fonts` / `wait_for_font_files` defer view creation until requested font folders/files have loaded (works around `CachedFontProvider`'s one-shot `ScanFolder` caching). Font fallback (`GUI::SetFontFallbacks`) and theme resources (`GUI::LoadApplicationResources`) install lazily once fonts are present.
- **Input** (`src/input.rs`, `NoesisInputPlugin`) — Bevy event forwarders push onto a `NoesisInputQueue` (extracted to render world); `apply_noesis_input` drains it onto the `View`. `key_map::from_bevy` maps `KeyCode`; unmapped keys still produce `Char` events from `KeyboardInput.text`. Coord conversion collapses window scale-factor + intermediate-vs-window size into one ratio.

## Known gaps / open work

- **Effects pipeline (largest open area).** Opacity / Shadow / Blur / Downsample / Upsample shaders are unimplemented. Scenes that need them (`Transform3D.xaml`, `Effects.xaml`) panic on `Shader(49)=DOWNSAMPLE`. This also covers offscreen render-target effects (blur, shadow, opacity groups, custom effects).
- **`SDF_LCD_SOLID`** — subpixel text needs dual-source blending (`@blend_src(1)` fragment output). Separate kickoff from `SDF_SOLID`.
- **Stencil not attached.** `create_render_target` allocates a stencil texture but no pipeline declares `depth_stencil`. Suspected cause of the **`ScrollViewer` content-viewport blank under theme** issue (`03_scroll.xaml` + `NOESIS_VIEWER_THEME=DarkBlue`: themed scrollbar chrome renders but the `ScrollContentPresenter` interior is flat white with content spilling its clip). Next step: log `draw_batch` (shader / render_state / RT / tile) for one real frame and compare against the `scrollviewer-no-theme` baseline in `tests/headless_offscreen_brush.rs`. Other suspect: `LoadOp::Load` on a freshly-created RT reading uninitialised memory.
- **PPAA + alpha blend.** `RenderFlag::Ppaa` anti-aliases edges to fractional alpha; with the blit's alpha-blending the camera clear color bleeds through. Runtime-toggleable via `NoesisScene.ppaa` (viewer `P` key) but off by default. Proper handling (premultiplied blit, or opaque-with-pre-clear) lands when text/effects demand AA.
- **`DrawingBrush` is unimplemented by Noesis itself** — SDK `Include/NsGui` has only `SolidColorBrush`, `ImageBrush`, `VisualBrush`, `LinearGradientBrush`, `RadialGradientBrush`. XAML using `<DrawingBrush>` silently drops the fill (zero draws — see `tests/headless_offscreen_brush.rs`). `VisualBrush` only paints when its `Visual` is in the logical tree. Real path for tiled visuals is `ImageBrush`.
- **Multi-view + hot-reload** — currently effectively one scene; multiple `NoesisView` entities and XAML hot-reload via Bevy asset-reload events are not wired.
- **Windows target** — `build.rs` is Linux-only; needs MSVC `Noesis.lib` import-library handling + DLL discovery/copy.
- **Direct-to-`ViewTarget` (perf)** — key `PipelineCache` on color format and retarget Noesis's pipelines at the camera's `ViewTarget` format so `NoesisNode` can drop the intermediate texture + blit pass.
- **Phase 5 corpus styling** — `assets/phase5/` Buttons set `Background`/`Foreground` without a `ControlTemplate`, so even with a theme loaded they show the magenta "no-Template" placeholder. Fix by `BasedOn` a theme Style or dropping the custom Style.

## Commands

- `cargo check -p dm_noesis_bevy`
- `cargo clippy -p dm_noesis_bevy --all-targets`
- `cargo test -p dm_noesis_runtime` — lifecycle smoke test (requires `NOESIS_SDK_DIR`)
- `cargo test -p dm_noesis_runtime --features test-utils` — adds the render-device integration test
- `cargo run -p dm_noesis_bevy --example xaml_viewer` — generalized viewer (defaults to `assets/phase5/`)
- `cargo run -p dm_noesis_bevy --example xaml_viewer <path>` — single file or directory. Keys: `[`/`]` cycle, `Home`/`End` jump, `R` reload, `S` screenshot, `P` toggle PPAA.
- `NOESIS_VIEWER_EXIT_AFTER=1 NOESIS_SCREENSHOT=out.png cargo run -p dm_noesis_bevy --example xaml_viewer <path>` — headless screenshot. `NOESIS_VIEWER_THEME=<name>` loads a theme; `NOESIS_VIEWER_IMAGES=<paths>` pre-loads images.

## Setup

See [`README.md`](./README.md) for `NOESIS_SDK_DIR` setup.
