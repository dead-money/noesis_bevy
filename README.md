# dm_noesis_bevy

Bevy 0.18 plugin for [Noesis GUI](https://www.noesisengine.com/). Boots Noesis at app startup, drives a `XamlAsset`-driven scene, implements `Noesis::RenderDevice` on top of Bevy's wgpu device, and composites the UI into a Bevy frame via a render-graph blit pass.

Sibling to the FFI crate [`dm_noesis_runtime`](https://github.com/dead-money/dm_noesis_runtime), which owns the C++ shim + Rust bindings.

> **About this project.** This crate is built for Dead Money's internal game projects and was primarily authored by AI agents (Claude Code) wrapping the Noesis Native SDK behind a Bevy plugin, with a human engineer directing scope, reviewing output, and steering architecture. It's published for transparency and for use inside Dead Money, not as a polished third-party library. Interfaces will shift, not everything is battle-tested, and documentation leans toward "what would a maintainer need?" rather than "what would a brand-new user expect?". If you adopt it anyway, expect to file issues and read source occasionally.

## You need a Noesis license to use this

This crate links against the Noesis Native SDK via [`dm_noesis_runtime`](https://github.com/dead-money/dm_noesis_runtime). The SDK is closed-source commercial software distributed by Noesis Technologies S.L. under their own EULA — neither dm_noesis_bevy nor dm_noesis_runtime redistributes it. You must obtain it separately and point `NOESIS_SDK_DIR` at your install. In practical terms:

- **Every developer building this crate needs the [Noesis Native SDK](https://www.noesisengine.com/) (Indie tier or higher).**
- **Distribution of binaries that link against the SDK is governed by your Noesis license** (Indie / Pro / Enterprise terms differ — see the [pricing page](https://www.noesisengine.com/pricing.php)).
- **`NOESIS_LICENSE_NAME` / `NOESIS_LICENSE_KEY` env vars suppress the trial watermark.** Without them the runtime works but renders a "trial" banner.

## What's in the box

- **`NoesisPlugin`** — boots Noesis at startup, installs the wgpu render device + every provider Noesis needs (XAML, font, texture), and shuts down cleanly on `App` drop. One `App::add_plugins(NoesisPlugin::default())` is everything for a basic scene.
- **`XamlAsset` + loader.** `.xaml` files load through Bevy's `AssetServer`; the bytes feed Noesis's parser via a `BevyXamlProvider` shim. `XamlRegistry` mirrors the loaded set into the render world.
- **`FontAsset` + loader + fallback chain.** `BevyFontProvider` subclasses Noesis's `CachedFontProvider`, so weight/stretch/style matching stays inside Noesis. `NoesisScene::font_fallbacks` declares the family precedence Noesis falls through when an explicit `FontFamily` doesn't match.
- **`ImageAsset` + loader.** PNG / JPEG decode via the `image` crate; PMA (premultiplied alpha) cooked at decode time so Noesis's default `BlendMode::SrcOver` `(One, OneMinusSrcAlpha)` doesn't fringe edges.
- **`WgpuRenderDevice`** — implements every `dm_noesis_runtime::render_device::RenderDevice` virtual against Bevy's wgpu device. Pipeline cache keyed on `(shader, render-state, vertex-format)` with lazy build. Unified `shaders/noesis.wgsl` covers the SDK shader matrix (`Path_*`, `Path_AA_*`, `SDF_*`, `RGBA`, `Mask`, `Clear`, `Pattern_*` and the AA variants, gradients, image brushes).
- **`NoesisRenderPlugin` + render-graph node** — extracts the scene + asset registries main→render every frame, drives `View::Update(time)` from a render-world wall clock, and blits the offscreen Noesis target into the camera's `ViewTarget` with sRGB-correct sampling.
- **Input forwarding.** Pointer / keyboard / touch / focus events route from Bevy's input layer into `View::MouseMove` / `MouseButton{Down,Up}` / `KeyDown` / `Char` / `TouchDown` / `Activate` / etc. Input runs in the render world to keep Noesis single-threaded.
- **Routed-event bridge.** `NoesisClickWatch` declares which `x:Name`s to subscribe; `NoesisClicked` events surface in the main world for Bevy systems to consume. Pattern generalizes — additional routed events plug in alongside `Click`.
- **Custom-class + markup-extension registries.** `NoesisClassRegistry` + `NoesisMarkupExtensionRegistry` resources own the `dm_noesis_runtime::classes::ClassRegistration` / `MarkupExtensionRegistration` instances for the app's lifetime. Drop ordering guarantees they clean up before `dm_noesis_runtime::shutdown`.
- **`xaml_viewer` example.** One-stop scene cycler — point it at a `.xaml` file or directory; `[`/`]` cycles, `R` reloads, `S` screenshots, `P` toggles PPAA. Runs headless under `NOESIS_VIEWER_EXIT_AFTER=1` for CI.

## What's explicitly out of scope

- **Effects.** Blur, shadow, opacity groups, `DOWNSAMPLE` / `UPSAMPLE`. Phase 6 work; not shipping yet. Rules out a few SDK samples (`Transform3D.xaml`, `Effects.xaml`).
- **`SDF_LCD_SOLID`** — needs dual-source blending. Tracked, not landed.
- **Multi-view + hot-reload.** One `NoesisScene` per app for now; XAML hot-reload via Bevy's asset events is a planned addition.
- **Windows.** The `build.rs` skeleton handles MSVC import-library + DLL discovery, but the crate has only been smoke-tested on Linux. Windows-specific bugs are likely.
- **Direct-to-`ViewTarget` rendering.** Currently composites through an intermediate texture + blit pass; bypassing that is a perf win for later.
- **Accessibility / IME.** Bevy's accesskit integration doesn't currently route into Noesis's accessibility tree. IME composition events for non-Latin text input are stubbed.

## Quick start

```toml
[dependencies]
bevy = "0.18"
dm_noesis_bevy = { git = "https://github.com/dead-money/dm_noesis_bevy" }
```

```rust
use bevy::prelude::*;
use dm_noesis_bevy::{NoesisPlugin, NoesisScene};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(NoesisPlugin::default())
        .add_systems(Startup, setup)
        .run();
}

fn setup(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.spawn(Camera2d);
    // Keep the handle alive — XAML asset GC otherwise drops the bytes
    // before Noesis's parser asks for them.
    let _xaml = asset_server.load::<dm_noesis_bevy::XamlAsset>("MainMenu.xaml");
    commands.insert_resource(NoesisScene {
        xaml_uri: "MainMenu.xaml".to_string(),
        size: UVec2::new(1920, 1080),
        ..default()
    });
}
```

`NoesisPlugin::default()` reads `NOESIS_LICENSE_NAME` / `NOESIS_LICENSE_KEY` from the environment if set; pass `NoesisLicense { name, key }` explicitly to override.

For a runnable demo with scene cycling, theme loading, and screenshot harness, see the **xaml_viewer example**:

```sh
# Cycle through assets/phase5/*.xaml — [/] navigate, R reload, S screenshot, P toggle PPAA
cargo run --example xaml_viewer

# Single XAML file
cargo run --example xaml_viewer -- path/to/scene.xaml

# Themed control gallery (loads the Noesis SDK's DarkBlue theme)
NOESIS_VIEWER_THEME=DarkBlue \
    cargo run --example xaml_viewer -- assets/Data/Styles.xaml

# Headless screenshot for CI
NOESIS_VIEWER_EXIT_AFTER=1 NOESIS_SCREENSHOT=/tmp/out.png NOESIS_SCREENSHOT_FRAMES=120 \
    cargo run --example xaml_viewer -- assets/phase5/08_radial.xaml
```

Viewer environment variables: `NOESIS_VIEWER_PATH`, `NOESIS_VIEWER_SIZE` (`WxH`), `NOESIS_VIEWER_THEME`, `NOESIS_VIEWER_IMAGES` (comma-separated asset paths to pre-load), `NOESIS_SCREENSHOT`, `NOESIS_SCREENSHOT_FRAMES`, `NOESIS_VIEWER_EXIT_AFTER`.

### Custom controls + markup extensions

Register Rust-backed XAML types from a `Startup` system, then load XAML that uses them:

```rust
use bevy::prelude::*;
use dm_noesis_bevy::classes::{
    ClassBase, ClassBuilder, NoesisClassRegistry, PropType,
    PropertyChangeHandler, PropertyValue, Instance,
};

struct NineSlicerHandler { source_idx: u32 /* ... */ }
impl PropertyChangeHandler for NineSlicerHandler {
    fn on_changed(&mut self, instance: Instance, idx: u32, value: PropertyValue<'_>) {
        // Recompute derived properties + write them back via instance.set_*().
    }
}

fn register(mut registry: ResMut<NoesisClassRegistry>) {
    let mut b = ClassBuilder::new("MyNs.NineSlicer", ClassBase::ContentControl,
                                  NineSlicerHandler { source_idx: 0 });
    b.add_property("Source", PropType::ImageSource);
    b.add_property("SliceThickness", PropType::Thickness);
    if let Some(reg) = b.register() { registry.add(reg); }
}
```

`MarkupExtensionRegistration` follows the same pattern via `NoesisMarkupExtensionRegistry`. See `dm_noesis_runtime`'s README for the FFI-level details.

## Design notes

- **Render-app boundary.** Noesis lives entirely in the render world. The main world owns `NoesisScene` (configuration), asset registries, and the input event source; everything Noesis touches (View, Renderer, RenderDevice, providers) is on the render side, behind `!Send` resources. Bevy 0.18's `ExtractResourcePlugin` mirrors the asset registries each frame.
- **Single intermediate texture, then blit.** `NoesisRenderState` allocates one offscreen `wgpu::Texture` sized to the scene; Noesis renders into that, then a `NoesisNode` graph node samples it into the camera's `ViewTarget` with the right sRGB conversion. Direct-to-`ViewTarget` rendering is a future perf knob.
- **`forbid(unsafe_code)`.** Every line of Bevy-side glue is safe Rust. All `unsafe` lives in the sibling `dm_noesis_runtime` crate behind type-checked safe wrappers.
- **PMA at decode time.** PNG/JPEG decode produces straight-alpha bytes; we premultiply once at load (`(c * a + 127) / 255`) so Noesis's `SrcOver` blend doesn't fringe transparent edges. The `ImageAssetLoader` is idempotent — never sees its own output.
- **Font fallback ordering.** Noesis's `CachedFontProvider` doesn't lazy-scan a folder when an explicit `FontFamily="Fonts/X.otf#X"` is set; if the family isn't already registered, Noesis falls through to the previously-installed fallback chain. Listing your primary font in `NoesisScene::font_fallbacks` forces the scan eagerly so explicit references resolve.
- **Lifecycle ordering.** `NoesisShutdownGuard` is a `!Send` non-send resource; Bevy 0.18 drops non-send resources after regular ones, so `ClassRegistration` / `MarkupExtensionRegistration` / `SceneInstance` all release their Noesis handles before the global `dm_noesis_runtime::shutdown()` fires.

## Setup

```sh
unzip NoesisGUI-NativeSDK-linux-3.2.12-Indie.zip -d ~/sdks/noesis-3.2.12
export NOESIS_SDK_DIR=~/sdks/noesis-3.2.12
export LD_LIBRARY_PATH=$NOESIS_SDK_DIR/Bin/linux_x86_64:$LD_LIBRARY_PATH
```

Symlink the SDK's font + data directories so the included examples + `NOESIS_VIEWER_THEME` loader find them (these `assets/` paths are gitignored):

```sh
ln -sfn $NOESIS_SDK_DIR/Data/Fonts assets/Fonts
ln -sfn $NOESIS_SDK_DIR/Data        assets/Data
```

Optional — apply your Indie credentials to suppress the trial watermark:

```sh
export NOESIS_LICENSE_NAME=...
export NOESIS_LICENSE_KEY=...
```

```sh
cargo test
cargo run --example xaml_viewer
```

## Layout

For maintainers — what lives where in the tree.

- `src/lib.rs` — `NoesisPlugin`, `NoesisLicense`, public re-exports.
- `src/render.rs` — `NoesisScene` resource, `NoesisRenderPlugin` (render-app side), `NoesisRenderState`, `NoesisNode` graph node, main↔render extraction systems, blit pipeline cache.
- `src/render_device/` — `WgpuRenderDevice` + pipeline cache + WGSL preprocessor + unified `shaders/noesis.wgsl`.
- `src/xaml.rs` — `XamlAsset` + loader + `XamlRegistry` + `BevyXamlProvider`.
- `src/font.rs` — `FontAsset` + loader + `FontRegistry` + `BevyFontProvider`.
- `src/image.rs` — `ImageAsset` + loader (PNG/JPEG via `image` crate) + `ImageRegistry` + `BevyTextureProvider` + `SharedImageMap`.
- `src/input.rs` — Bevy event → `NoesisInputQueue` forwarders + render-world ingestion.
- `src/events.rs` — `NoesisClickWatch` / `NoesisClicked` + the `BaseButton::Click` subscription bridge.
- `src/classes.rs` — `NoesisClassRegistry` + `NoesisClassPlugin` (custom XAML class lifecycle).
- `src/markup.rs` — `NoesisMarkupExtensionRegistry` + `NoesisMarkupExtensionPlugin` (custom MarkupExtension lifecycle).
- `assets/phase5/*.xaml` — Phase 5 input + animation corpus (button hover/click, scroll, textbox, storyboard, touch, radial, tiled pattern, image brush).
- `examples/xaml_viewer.rs` — generalized viewer (the headline example).
- `examples/phase4_visual.rs`, `examples/bevy_wgpu_bridge.rs` — lower-level device / graph-node smoke tests.
- `tests/` — integration tests: `wgpu_first_triangle`, `wgpu_multi_shader`, `wgpu_uniform_ring`, `wgpu_offscreen_rt`, `wgpu_pattern`, `wgpu_pattern_wrap`, `wgpu_radial`, `headless_xaml`, `headless_xaml_nested`, `headless_offscreen_brush`.
- `CLAUDE.md` — phase tracker + architectural invariants. Read first when contributing.
- `docs/PHASE_5_PLAN.md` — retrospective on the input + animation slice.
- Sibling crate [`dm_noesis_runtime`](https://github.com/dead-money/dm_noesis_runtime) — narrow C++ shim + Rust FFI to libNoesis (`RenderDevice`, `XamlProvider`, `FontProvider`, `TextureProvider`, `View`, `Renderer`, input, custom classes + MarkupExtensions).

## Licensing

Source in this repository is © 2026 Dead Money, distributed under the [MIT License](./LICENSE). Every file under `src/`, `tests/`, `examples/`, `assets/`, and `docs/` is original work — no Noesis SDK code is vendored. The SDK is referenced via `dm_noesis_runtime` and pulled in at build time only.

The Noesis Native SDK itself is **not redistributed** here. You must obtain it from Noesis Technologies under their own EULA; the sibling `dm_noesis_runtime` crate's `build.rs` reads it from `NOESIS_SDK_DIR` at compile time and links `libNoesis.{so,dll,dylib}` from `Bin/<platform>/`. Use, distribution, and licensing of binaries you build that link against the SDK are governed by the Noesis EULA, not by the MIT License above.

## Acknowledgements

Built on top of [Bevy](https://bevy.org/) and [Noesis Technologies](https://www.noesisengine.com/)' Native SDK. The upstream documentation at [docs.noesisengine.com](https://docs.noesisengine.com/) remains the source of truth for XAML behaviour, control templates, and binding semantics — protocol or runtime bugs in the underlying SDK should be reported there; Bevy-integration bugs should be filed here.
