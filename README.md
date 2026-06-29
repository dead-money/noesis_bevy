# noesis_bevy

[![CI](https://github.com/dead-money/noesis_bevy/actions/workflows/ci.yml/badge.svg)](https://github.com/dead-money/noesis_bevy/actions/workflows/ci.yml)

A Bevy 0.18 plugin for [Noesis GUI](https://www.noesisengine.com/). It boots Noesis at startup, renders a XAML scene through an implementation of `Noesis::RenderDevice` on Bevy's wgpu device, and composites the result into your Bevy frame.

It pairs with the FFI crate [`noesis_runtime`](https://github.com/dead-money/noesis_runtime), which owns the C++ shim and Rust bindings to the SDK. All `unsafe` lives there; this crate is `forbid(unsafe_code)`.

Built for Dead Money's own games and mostly written by AI agents under human direction.

<p align="center">
  <img src="docs/scoreboard.png" alt="Noesis Scoreboard sample rendered in Bevy" width="820">
</p>
<p align="center"><em>The Noesis Scoreboard sample, rendered live in a Bevy frame through noesis_bevy. Every value (the team scores, the per-player table, the team filter) flows in through the crate's safe binding bridges.</em></p>

## You need a Noesis license

This crate links against the [Noesis Native SDK](https://www.noesisengine.com/), closed-source commercial software from Noesis Technologies S.L. We do not redistribute it. Every developer needs their own copy (Indie tier or higher); obtain it separately and point `NOESIS_SDK_DIR` at your install. `noesis_runtime`'s `build.rs` reads it at compile time and links `libNoesis` from the matching `Bin/<platform>/` directory.

This release targets **Noesis Native SDK 3.2.13**. The C ABI shim is compiled against that version's headers and checks key struct sizes at build time, so a different SDK version may fail to build or link. Match it unless you have verified a newer release.

Set `NOESIS_LICENSE_NAME` and `NOESIS_LICENSE_KEY` to your credentials. Without them the runtime works for a while, then blanks the view with a "Trial expired" message.

## Quick start

```toml
[dependencies]
bevy = "0.18"
noesis_bevy = { git = "https://github.com/dead-money/noesis_bevy" }
```

It is a git dependency, not a crates.io release, and it still links the Noesis SDK at build time. You need `NOESIS_SDK_DIR` set (see above) for it to compile.

```rust
use bevy::prelude::*;
use noesis_bevy::{NoesisPlugin, NoesisScene};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(NoesisPlugin::default())
        .add_systems(Startup, setup)
        .run();
}

fn setup(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.spawn(Camera2d);
    // Keep the handle alive. Otherwise asset GC drops the XAML bytes
    // before Noesis's parser asks for them.
    let _xaml = asset_server.load::<noesis_bevy::XamlAsset>("MainMenu.xaml");
    commands.insert_resource(NoesisScene {
        xaml_uri: "MainMenu.xaml".to_string(),
        size: UVec2::new(1920, 1080),
        ..default()
    });
}
```

`NoesisPlugin::default()` reads `NOESIS_LICENSE_NAME` and `NOESIS_LICENSE_KEY` from the environment. Pass `NoesisLicense { name, key }` to set them explicitly.

## The Scoreboard sample

```sh
cargo run --example scoreboard
```

`scoreboard` is the flagship example: a faithful port of Noesis's own Scoreboard demo, driven entirely through the crate's safe bridges. The sample's real `MainWindow.xaml` (the emblem geometries, gradient and radial brushes, control templates, the per-player `DataTemplate` with its `DataTrigger`s, and the `b:Interaction` behaviors) is the SDK's own XAML, parsed byte for byte. The data flows in without raw FFI:

- `NoesisVm` attaches a `DependencyObject`-backed `Scoreboard.Game` view model as the view-root `DataContext`, supplying the scalar bindings (`Name`, `AllianceScore`, `HordeScore`, `ElapsedTime`, `SelectedTeam`).
- `NoesisItems::with_objects` fills the per-player `ItemsControl` with ten bindable object rows (one Rust-backed Noesis class per row), so the per-player `DataTemplate` bindings resolve and the team/class `DataTrigger`s fire.
- `NoesisItems::with` fills the team-filter `ComboBox`, whose `SelectedIndex` binds two-way to `Game.SelectedTeam`; `NoesisDp` reads the bound `SelectedIndex` back.
- `NoesisWindowCompatPlugin` registers a content-host stand-in for the sample's `<Window>` root, an App-framework type absent from the core runtime, so the genuine XAML parses unmodified.

The sample reads the SDK's real `MainWindow.xaml` and its two fonts from `$NOESIS_SDK_DIR` at runtime (nothing is vendored), so it needs that variable set. When it is unset, the example skips gracefully with a warning. The headless data round-trip (the ten players reaching the control, and a mid-run `SelectedTeam` edit surfacing on the combo's `SelectedIndex`) is asserted by `tests/headless_example_scoreboard.rs`.

```sh
# Headless screenshot:
NOESIS_VIEWER_EXIT_AFTER=1 NOESIS_SCREENSHOT=scoreboard.png \
    cargo run --example scoreboard
```

## The viewer example

`xaml_viewer` is a runnable demo with scene cycling, theme loading, and a screenshot harness:

```sh
# Cycle through assets/viewer_samples/*.xaml. [/] navigate, R reload, S screenshot, P toggle PPAA.
cargo run --example xaml_viewer

# A single XAML file
cargo run --example xaml_viewer -- path/to/scene.xaml

# A themed control gallery (loads the SDK's DarkBlue theme)
NOESIS_VIEWER_THEME=DarkBlue \
    cargo run --example xaml_viewer -- assets/Data/Styles.xaml

# A headless screenshot for CI
NOESIS_VIEWER_EXIT_AFTER=1 NOESIS_SCREENSHOT=/tmp/out.png NOESIS_SCREENSHOT_FRAMES=120 \
    cargo run --example xaml_viewer -- assets/viewer_samples/08_radial.xaml
```

Environment variables: `NOESIS_VIEWER_PATH`, `NOESIS_VIEWER_SIZE` (`WxH`), `NOESIS_VIEWER_THEME`, `NOESIS_VIEWER_IMAGES` (comma-separated asset paths to pre-load), `NOESIS_SCREENSHOT`, `NOESIS_SCREENSHOT_FRAMES`, `NOESIS_VIEWER_EXIT_AFTER`.

## Custom controls and markup extensions

Register Rust-backed XAML types from a `Startup` system, then load XAML that uses them:

```rust
use bevy::prelude::*;
use noesis_bevy::classes::{
    ClassBase, ClassBuilder, NoesisClassRegistry, PropType,
    PropertyChangeHandler, PropertyValue, Instance,
};

struct NineSlicerHandler { source_idx: u32 /* ... */ }
impl PropertyChangeHandler for NineSlicerHandler {
    fn on_changed(&mut self, instance: Instance, idx: u32, value: PropertyValue<'_>) {
        // Recompute derived properties and write them back via instance.set_*().
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

`MarkupExtensionRegistration` works the same way via `NoesisMarkupExtensionRegistry`. See the `noesis_runtime` README for the FFI-level details.

## Data binding

Bind a plain Bevy `Resource` to XAML `{Binding field_name}`: derive `NoesisViewModel`, register it, and each field is reflected to the binding engine by name, two-way.

```rust
use bevy::prelude::*;
use noesis_bevy::{NoesisPlugin, NoesisViewModel, NoesisViewModelAppExt};

#[derive(Resource, NoesisViewModel)]
struct SettingsVm {
    volume: f32,   // <Slider Value="{Binding volume, Mode=TwoWay}"/>
    muted: bool,   // <CheckBox IsChecked="{Binding muted}"/>
    quality: i32,  // <ComboBox SelectedIndex="{Binding quality, Mode=TwoWay}"/>
}

App::new()
    .add_plugins((DefaultPlugins, NoesisPlugin::default()))
    .insert_resource(SettingsVm { volume: 0.8, muted: false, quality: 2 })
    .add_noesis_view_model::<SettingsVm>(); // attach as the view-root DataContext
```

Mutating the resource updates the bound controls (Bevy change detection drives `INotifyPropertyChanged`); a control edit writes back into the resource. Supported field types are `f32`/`f64`, `i32`/`u32`, `bool`, and `String`; mark other fields `#[noesis(skip)]`.

For lower-level access there are also `NoesisVm` (a `DependencyObject`-backed view model with explicit properties), `NoesisItems` (drive an `ItemsControl`/`ComboBox`/`ListBox` from a Rust collection, with `with_objects` for bindable object rows), and `NoesisDp` (binding-free get/set/watch of any dependency property by `(x:Name, property)`).

## How it works

- **The bridge pattern.** A feature is a `#[derive(Component)]` on the `NoesisView` camera entity plus a reconcile system in `NoesisSet::Apply` that calls into that view's live scene; read-backs surface as a `Message { view, ... }`. `NoesisVm`, `NoesisItems`, `NoesisDp`, `NoesisText`, and the rest follow the same shape, so adding a capability means mirroring an existing bridge module.
- **Noesis lives in the render world.** The main world owns the `NoesisScene` config, the asset registries, and the input event source. Everything Noesis touches (View, Renderer, RenderDevice, providers) sits on the render side behind `!Send` resources. Bevy's `ExtractResourcePlugin` mirrors the asset registries each frame.
- **One intermediate texture, then a blit.** Noesis renders into an offscreen texture sized to the scene. A `NoesisNode` graph node then samples that into the camera's `ViewTarget` with the correct sRGB conversion. Rendering straight to `ViewTarget` is a future optimization.
- **No unsafe here.** This crate is `forbid(unsafe_code)`. All `unsafe` lives in `noesis_runtime` behind type-checked safe wrappers.
- **Premultiplied alpha at decode time.** PNG and JPEG decode to straight alpha; we premultiply once at load so Noesis's `SrcOver` blend does not fringe transparent edges. The loader is idempotent and never sees its own output.
- **Font fallback ordering.** Noesis's `CachedFontProvider` will not lazy-scan a folder for an explicit `FontFamily` reference. Listing your primary font in `NoesisScene::font_fallbacks` forces the scan early so those references resolve.

## Setup

```sh
unzip NoesisGUI-NativeSDK-linux-3.2.13-Indie.zip -d ~/sdks/noesis-3.2.13
export NOESIS_SDK_DIR=~/sdks/noesis-3.2.13
export LD_LIBRARY_PATH=$NOESIS_SDK_DIR/Bin/linux_x86_64:$LD_LIBRARY_PATH
```

Symlink the SDK's font and data directories so the examples and `NOESIS_VIEWER_THEME` loader can find them (these `assets/` paths are gitignored):

```sh
ln -sfn $NOESIS_SDK_DIR/Data/Fonts assets/Fonts
ln -sfn $NOESIS_SDK_DIR/Data        assets/Data
```

Apply your Noesis credentials so the runtime runs licensed:

```sh
export NOESIS_LICENSE_NAME=...
export NOESIS_LICENSE_KEY=...
```

Then build and run:

```sh
cargo test
cargo run --example xaml_viewer
```

## Licensing

Source in this repository is © 2026 Dead Money under the [MIT License](./LICENSE). Everything under `src/`, `tests/`, `examples/`, and `assets/` is original work; no Noesis SDK code is vendored.

The Noesis Native SDK is not redistributed here. You obtain it from Noesis Technologies under their EULA, and `noesis_runtime`'s `build.rs` links it from `NOESIS_SDK_DIR` at compile time. Use and distribution of binaries you build that link the SDK are governed by the Noesis EULA, not by the MIT License above.

## Acknowledgements

Built on [Bevy](https://bevy.org/) and the [Noesis](https://www.noesisengine.com/) Native SDK. The upstream docs at [docs.noesisengine.com](https://docs.noesisengine.com/) are the source of truth for XAML, control templates, and binding behavior. Report SDK bugs there; report integration bugs here.
</content>
</invoke>
