# dm_noesis_bevy

Bevy plugin that drives the closed-source Noesis GUI Native SDK and renders its UIs into a Bevy frame via a wgpu-backed `Noesis::RenderDevice`. Two crates plus the SDK:

- **This crate** (`dm_noesis_bevy/`) — Bevy integration: plugin, asset loaders, the wgpu render-device on the Rust side of the FFI vtable, and a render-graph blit pass that composites into Bevy. **`unsafe_code = forbid`.**
- **Sibling crate** [`../dm_noesis_runtime/`](https://github.com/dead-money/dm_noesis_runtime) — narrow C++ shim + Rust FFI to libNoesis; `unsafe` allowed. We own and maintain it; it's freely editable. Most new features start here: expand the shim to expose a Noesis capability, then build the Bevy glue on top. When a feature needs something libNoesis offers but the shim doesn't wrap yet, add it to the shim first rather than working around its absence.
- **Noesis Native SDK** (`$NOESIS_SDK_DIR`, e.g. `~/sdk/noesis-3.2.12`) — per-developer-licensed. **NEVER commit** any SDK content.

## Attribution

Do not add `Co-Authored-By: Claude` trailers to commits or "Generated with Claude Code" footers to PR bodies. Author lines and PR bodies stay clean.

## Reference material

- `RenderDevice` contract: `$NOESIS_SDK_DIR/Include/NsRender/{RenderDevice,Texture,RenderTarget}.h`. The frame protocol is in the header — read it first.
- Reference impl to mirror: `$NOESIS_SDK_DIR/Src/Packages/Render/GLRenderDevice/`. Our WGSL shaders port `Shader.140.{vert,frag}`.
- Integration reference (no `SetProjectionMatrix`, frame-driving order): `$NOESIS_SDK_DIR/Src/Packages/App/IntegrationGLUT/Src/Main.cpp`.
- Sample XAML for visual tests: `$NOESIS_SDK_DIR/Data/` (symlinked to `assets/Data/`).

## Architectural invariants

Deviating from these is a design change — raise it before implementing.

- **NOT a port of Noesis.** We FFI to `libNoesis.so` and author only (a) the wgpu render device and (b) the Bevy glue. The XAML parser, layout, controls, animation, and data binding all live inside libNoesis.
- **Two-crate split.** `dm_noesis_runtime` is the FFI quarantine (`unsafe` allowed); `dm_noesis_bevy` is `unsafe_code = forbid`.
- **Hand-written C ABI shim.** `dm_noesis_runtime/cpp/noesis_shim.{h,cpp}` is the only thing Rust binds against. **Do NOT bindgen** `NsCore`/`NsGui` — templates + `Ptr<T>` + virtual dispatch don't translate cleanly.
- **C++ subclass + Rust vtable is the FFI pattern.** Pure-virtual Noesis interfaces can't be implemented from Rust directly. The shim defines a C++ subclass (`RustRenderDevice`, `RustXamlProvider`, `RustFontProvider`, `RustTextureProvider`) whose virtuals trampoline into a Rust vtable + `void* userdata`. New hooks follow this pattern, each exposing a safe `set_*`/`Registered` wrapper with a `TypeId`-checked downcast accessor (`provider_mut::<P>()` / `device_mut::<D>()`).
- **Render-graph integration.** Each scene owns a graph-node-owned intermediate texture (`Rgba8Unorm`) that Noesis paints into; a fullscreen-triangle blit pass in `NoesisNode` (between `MainTransparentPass` and `EndMainPass`) copies it into the camera's `ViewTarget::main_texture_view`.
- **Render-app thread ownership.** `View::Update(time)` and `Renderer::Render()` run on the render-app thread. `View`/`Renderer` wrappers are `!Send` and live as non-send resources (`NoesisRenderState`) in the render world. Global `init()`/`shutdown()` stay on the main thread via `NoesisShutdownGuard`; render-app `Registered` guards must drop before that guard. `NoesisRenderState::drop` enforces `Renderer::shutdown` → `View` drop → device guard → provider guard, and must run on the render thread.
- **Clock source.** `NoesisRenderState` holds its own `std::time::Instant` origin and feeds `elapsed().as_secs_f64()` into `View::Update`. Do **not** use `bevy::time::Time<Real>` — Bevy 0.18 doesn't extract it to the render world (reads `0.0` forever → animations never advance).
- **Linear/sRGB and clip space** must match Bevy/wgpu. `DeviceCaps::linearRendering = false` (Noesis writes sRGB bytes into the `Rgba8Unorm` intermediate); the blit samples through an `Rgba8UnormSrgb` alias view when `ViewTarget` is sRGB (requires the sRGB format in the texture's `view_formats`). Set `clipSpaceYInverted` / `depthRangeZeroToOne` from `DeviceCaps`; do **not** call `View::SetProjectionMatrix` (a GL-style ortho makes Noesis's visibility pass cull child elements). WGSL projection is `v * M` to match Noesis's row-major `Matrix4` against WGSL column-major `mat4x4<f32>`.
- **SDK never in repo.** `NOESIS_SDK_DIR` env var; `build.rs` panics with a clear message when unset.

## How it fits together

- **Render device** (`src/render_device/wgpu_device.rs`) — `WgpuRenderDevice` implements the Rust side of the FFI vtable. `PipelineCache` keyed on `(shader, render_state, vertex_format)` with lazy build; vertex-layout dispatch from the SDK tables; a tiny `#ifdef` WGSL preprocessor (`shader_preproc.rs`) specializes the unified `noesis.wgsl`. `UniformRing` + dynamic-offset bind groups stream per-batch uniforms. group(2) holds the paint texture (`pattern`/`ramps`/`glyphs`) + sampler, gated by `HAS_PAINT_TEXTURE`-style defines.
- **Shaders implemented:** `Path_Solid`, `Path_AA_Solid`, `Mask`, `RGBA`, `Clear`; `PATH_PATTERN` (+AA) with CLAMP/REPEAT/MIRROR_{U,V}/MIRROR wrap variants; `PATH_LINEAR`/`PATH_RADIAL` (+AA, share the `ramps` texture); `SDF_SOLID` (glyphs). The `BlendMode` → `wgpu::BlendState` matrix covers Src / SrcOver / SrcOverMultiply / SrcOverScreen / SrcOverAdditive.
- **Providers** (shim + Bevy bridge) — XAML (`BevyXamlProvider` + `XamlRegistry`, `src/xaml.rs`), fonts (`BevyFontProvider` + `FontRegistry`, `src/font.rs`, `.ttf`/`.otf`/`.ttc`), images (`BevyTextureProvider` + `ImageRegistry`, `src/image.rs`, PNG/JPEG). Each registry is an `ExtractResource` (Arc-level clones) extracted main→render; the render side holds a `Mutex`-guarded map that never crosses the boundary. `LoadTexture` hands decoded RGBA8 to C++, which calls the same `RenderDevice` so images become real wgpu textures in `Batch.pattern`.
- **Scene lifecycle** (`src/render.rs`) — `NoesisScene` describes a scene (URI, size, flags, theme, font-wait gates). `ensure_noesis_scene` builds/resizes the `View` (resize reuses the View: `set_size` + rebuild intermediate); `drive_noesis_frame` runs `Update` → `UpdateRenderTree` → `RenderOffscreen` → `Render`. The `wait_for_fonts`/`wait_for_font_files` gates defer view creation until requested fonts load (works around `CachedFontProvider`'s one-shot `ScanFolder`). Font fallback and theme resources install lazily once fonts are present.
- **Input** (`src/input.rs`, `NoesisInputPlugin`) — Bevy event forwarders push onto a `NoesisInputQueue` (extracted to render world); `apply_noesis_input` (in `render.rs`) drains it onto the `View`. `key_map::from_bevy` maps `KeyCode`; unmapped keys still produce `Char` events from `KeyboardInput.text`. Coord conversion collapses window scale-factor + intermediate-vs-window size into one ratio.

## Known gaps / open work

- **Effects pipeline (largest open area).** Opacity / Shadow / Blur / Downsample / Upsample shaders are unimplemented; scenes needing them (`Transform3D.xaml`, `Effects.xaml`) panic on `Shader(49)=DOWNSAMPLE`. Also covers offscreen render-target effects.
- **`SDF_LCD_SOLID`** — subpixel text needs dual-source blending (`@blend_src(1)`). Separate from `SDF_SOLID`.
- **Stencil not attached.** `create_render_target` allocates a stencil texture but no pipeline declares `depth_stencil`. Suspected cause of the **`ScrollViewer` content-viewport blank under theme** issue (`03_scroll.xaml` + `NOESIS_VIEWER_THEME=DarkBlue`: scrollbar chrome renders but the content interior is flat white, spilling its clip). Next step: log `draw_batch` for one frame and compare against the `scrollviewer-no-theme` baseline in `tests/headless_offscreen_brush.rs`. Other suspect: `LoadOp::Load` on a fresh RT reading uninitialised memory.
- **PPAA + alpha blend.** `RenderFlag::Ppaa` produces fractional-alpha edges; with the blit's alpha-blending the camera clear color bleeds through. Toggleable via `NoesisScene.ppaa` (viewer `P` key), off by default. Proper handling (premultiplied blit, or opaque-with-pre-clear) lands when text/effects demand AA.
- **`DrawingBrush` is unimplemented by Noesis itself** — SDK has only `SolidColorBrush`, `ImageBrush`, `VisualBrush`, `LinearGradientBrush`, `RadialGradientBrush`. XAML using `<DrawingBrush>` silently drops the fill. `VisualBrush` only paints when its `Visual` is in the logical tree. Real path for tiled visuals is `ImageBrush`.
- **Multi-view + hot-reload** — effectively one scene today; multiple `NoesisView` entities and XAML asset-reload aren't wired.
- **Windows target** — `build.rs` is Linux-only; needs MSVC `Noesis.lib` handling + DLL discovery/copy.
- **Direct-to-`ViewTarget` (perf)** — key `PipelineCache` on color format and target the camera's `ViewTarget` format so `NoesisNode` can drop the intermediate + blit.
- **Phase 5 corpus styling** — `assets/phase5/` Buttons set `Background`/`Foreground` without a `ControlTemplate`, so even themed they show the magenta no-Template placeholder. Fix by `BasedOn` a theme Style or dropping the custom Style.

## Commands

- `cargo check -p dm_noesis_bevy`
- `cargo clippy -p dm_noesis_bevy --all-targets`
- `cargo test -p dm_noesis_runtime` — lifecycle smoke test (requires `NOESIS_SDK_DIR`)
- `cargo test -p dm_noesis_runtime --features test-utils` — adds the render-device integration test
- `cargo run -p dm_noesis_bevy --example xaml_viewer [path]` — viewer; defaults to `assets/phase5/`. `path` is a file or directory. Keys: `[`/`]` cycle, `Home`/`End` jump, `R` reload, `S` screenshot, `P` toggle PPAA.
- `NOESIS_VIEWER_EXIT_AFTER=1 NOESIS_SCREENSHOT=out.png cargo run -p dm_noesis_bevy --example xaml_viewer <path>` — headless screenshot. Also: `NOESIS_VIEWER_THEME=<name>`, `NOESIS_VIEWER_IMAGES=<paths>`.

## Setup

See [`README.md`](./README.md) for `NOESIS_SDK_DIR` setup.
