# dm_noesis_bevy

A Bevy 0.18 plugin for [Noesis GUI](https://www.noesisengine.com/). It boots Noesis at startup, renders a XAML scene through an implementation of `Noesis::RenderDevice` on Bevy's wgpu device, and composites the result into your Bevy frame.

It pairs with the FFI crate [`dm_noesis_runtime`](https://github.com/dead-money/dm_noesis_runtime), which owns the C++ shim and Rust bindings to the SDK.

Built for Dead Money's own games and mostly written by AI agents under human direction. We publish it for transparency and internal use, so expect changing interfaces and rough edges.

## You need a Noesis license

This crate links against the [Noesis Native SDK](https://www.noesisengine.com/), closed-source commercial software from Noesis Technologies S.L. We do not redistribute it. Every developer needs their own copy (Indie tier or higher); obtain it separately and point `NOESIS_SDK_DIR` at your install.

Set `NOESIS_LICENSE_NAME` and `NOESIS_LICENSE_KEY` to your credentials. Without them the runtime runs unlicensed and eventually blanks the view with a "Trial expired" message.

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
    // Keep the handle alive. Otherwise asset GC drops the XAML bytes
    // before Noesis's parser asks for them.
    let _xaml = asset_server.load::<dm_noesis_bevy::XamlAsset>("MainMenu.xaml");
    commands.insert_resource(NoesisScene {
        xaml_uri: "MainMenu.xaml".to_string(),
        size: UVec2::new(1920, 1080),
        ..default()
    });
}
```

`NoesisPlugin::default()` reads `NOESIS_LICENSE_NAME` and `NOESIS_LICENSE_KEY` from the environment. Pass `NoesisLicense { name, key }` to set them explicitly.

## The viewer example

`xaml_viewer` is a runnable demo with scene cycling, theme loading, and a screenshot harness:

```sh
# Cycle through assets/phase5/*.xaml. [/] navigate, R reload, S screenshot, P toggle PPAA.
cargo run --example xaml_viewer

# A single XAML file
cargo run --example xaml_viewer -- path/to/scene.xaml

# A themed control gallery (loads the SDK's DarkBlue theme)
NOESIS_VIEWER_THEME=DarkBlue \
    cargo run --example xaml_viewer -- assets/Data/Styles.xaml

# A headless screenshot for CI
NOESIS_VIEWER_EXIT_AFTER=1 NOESIS_SCREENSHOT=/tmp/out.png NOESIS_SCREENSHOT_FRAMES=120 \
    cargo run --example xaml_viewer -- assets/phase5/08_radial.xaml
```

Environment variables: `NOESIS_VIEWER_PATH`, `NOESIS_VIEWER_SIZE` (`WxH`), `NOESIS_VIEWER_THEME`, `NOESIS_VIEWER_IMAGES` (comma-separated asset paths to pre-load), `NOESIS_SCREENSHOT`, `NOESIS_SCREENSHOT_FRAMES`, `NOESIS_VIEWER_EXIT_AFTER`.

## Custom controls and markup extensions

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

`MarkupExtensionRegistration` works the same way via `NoesisMarkupExtensionRegistry`. See the `dm_noesis_runtime` README for the FFI-level details.

## Data binding

Bind a plain Bevy `Resource` to XAML `{Binding field_name}` — derive `NoesisViewModel`, register it, and each field is reflected to the binding engine by name, two-way:

```rust
use bevy::prelude::*;
use dm_noesis_bevy::{NoesisPlugin, NoesisViewModel, NoesisViewModelAppExt};

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

Mutating the resource updates the bound controls (Bevy change detection → `INotifyPropertyChanged`); a control edit writes back into the resource. Supported field types are `f32`/`f64`, `i32`/`u32`, `bool`, and `String`; mark other fields `#[noesis(skip)]`.

For lower-level access there are also `NoesisViewModels` (a `DependencyObject`-backed view model with explicit properties), `NoesisItemsSources` (drive a `ComboBox`/`ListBox`'s items from a Rust `ObservableCollection`), and `NoesisDpRequests` / `NoesisDpReadWatch` (binding-free get/set of any dependency property by `(x:Name, property)`).

## How it works

- **Noesis lives in the render world.** The main world owns the `NoesisScene` config, the asset registries, and the input event source. Everything Noesis touches (View, Renderer, RenderDevice, providers) sits on the render side behind `!Send` resources. Bevy's `ExtractResourcePlugin` mirrors the asset registries each frame.
- **One intermediate texture, then a blit.** Noesis renders into an offscreen texture sized to the scene. A `NoesisNode` graph node then samples that into the camera's `ViewTarget` with the correct sRGB conversion. Rendering straight to `ViewTarget` is a future optimization.
- **No unsafe here.** This crate is `forbid(unsafe_code)`. All `unsafe` lives in `dm_noesis_runtime` behind type-checked safe wrappers.
- **Premultiplied alpha at decode time.** PNG and JPEG decode to straight alpha; we premultiply once at load so Noesis's `SrcOver` blend doesn't fringe transparent edges. The loader is idempotent and never sees its own output.
- **Font fallback ordering.** Noesis's `CachedFontProvider` won't lazy-scan a folder for an explicit `FontFamily` reference. Listing your primary font in `NoesisScene::font_fallbacks` forces the scan early so those references resolve.

## Setup

```sh
unzip NoesisGUI-NativeSDK-linux-3.2.12-Indie.zip -d ~/sdks/noesis-3.2.12
export NOESIS_SDK_DIR=~/sdks/noesis-3.2.12
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

The Noesis Native SDK is not redistributed here. You obtain it from Noesis Technologies under their EULA, and `dm_noesis_runtime`'s `build.rs` links it from `NOESIS_SDK_DIR` at compile time. Use and distribution of binaries you build that link the SDK are governed by the Noesis EULA, not by the MIT License above.

## Acknowledgements

Built on [Bevy](https://bevy.org/) and the [Noesis](https://www.noesisengine.com/) Native SDK. The upstream docs at [docs.noesisengine.com](https://docs.noesisengine.com/) are the source of truth for XAML, control templates, and binding behavior. Report SDK bugs there; report integration bugs here.
