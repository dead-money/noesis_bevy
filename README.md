# noesis_bevy

[![CI](https://github.com/dead-money/noesis_bevy/actions/workflows/ci.yml/badge.svg)](https://github.com/dead-money/noesis_bevy/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/noesis_bevy.svg)](https://crates.io/crates/noesis_bevy)
[![docs.rs](https://img.shields.io/docsrs/noesis_bevy)](https://docs.rs/noesis_bevy)

A Bevy 0.18 plugin that renders [Noesis GUI](https://www.noesisengine.com/) XAML-driven UI into your frame. Noesis draws the scene on Bevy's own GPU; the plugin composites the result onto a camera.

It builds on the FFI crate [`noesis_runtime`](https://github.com/dead-money/noesis_runtime), which wraps the C++ SDK. All `unsafe` lives there. This crate has none of its own and sets `#![forbid(unsafe_code)]`.

Built for Dead Money's own games and mostly written by AI agents under human direction.

<p align="center">
  <img src="docs/scoreboard.png" alt="Noesis Scoreboard sample rendered in Bevy" width="820">
</p>
<p align="center"><em>The Noesis Scoreboard sample, rendered live in a Bevy frame through noesis_bevy. Every value (the team scores, the per-player table, the team filter) flows in through the crate's binding bridges.</em></p>

## You need a Noesis license

This crate links against the [Noesis Native SDK](https://www.noesisengine.com/), closed-source commercial software we don't redistribute. Buy it separately (Indie tier or higher) and point `NOESIS_SDK_DIR` at your install; the build links against it from there.

This release targets **Noesis Native SDK 3.2.13** and is compiled against that version's headers, so a different SDK version may not link. Match it unless you've verified a newer one.

Supported targets are **Linux** (`x86_64`, `aarch64`) and **Windows** (`x86_64-pc-windows-msvc`). Linux is the primary target; Windows support is newer.

Set `NOESIS_LICENSE_NAME` and `NOESIS_LICENSE_KEY` to apply your license. Without them the UI runs for a while, then blanks the view with a "Trial expired" message.

## Quick start

```toml
[dependencies]
bevy = "0.18"
noesis_bevy = "0.10"
```

It links the Noesis SDK at build time, so you need `NOESIS_SDK_DIR` set (see above) to compile.

```rust
use std::sync::Arc;
use bevy::prelude::*;
use noesis_bevy::{NoesisCamera, NoesisPlugin, NoesisView, XamlRegistry};

const MENU_XAML: &str = r#"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation">
  <TextBlock Text="Hello, Noesis!" Foreground="White"
             HorizontalAlignment="Center" VerticalAlignment="Center"/>
</Grid>"#;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(NoesisPlugin::default())
        .add_systems(Startup, setup)
        .run();
}

fn setup(mut commands: Commands, mut xaml: ResMut<XamlRegistry>) {
    // Register XAML by URI. For real projects, load .xaml files through the
    // asset server instead; the loader feeds the same registry.
    xaml.insert("menu.xaml", Arc::new(MENU_XAML.as_bytes().to_vec()));

    // A view is a `NoesisView` component on a 2D camera tagged `NoesisCamera`.
    // Noesis renders the scene offscreen and composites it onto that camera.
    commands.spawn((
        Camera2d,
        NoesisCamera,
        NoesisView {
            xaml_uri: "menu.xaml".to_string(),
            size: UVec2::new(1920, 1080),
            ..default()
        },
    ));
}
```

`NoesisPlugin::default()` reads `NOESIS_LICENSE_NAME` and `NOESIS_LICENSE_KEY` from the environment. Pass `NoesisLicense { name, key }` to set them explicitly.

## How the UI fits Bevy

The mental model is closer to "Bevy hosts an embedded retained-mode GUI runtime and renders its output" than "Bevy UI describes the widgets." XAML is authoritative for structure and layout; the ECS supplies data and intent across a small typed bridge surface.

- **The XAML tree lives inside Noesis, not the ECS.** Noesis parses your XAML and owns the live tree of controls (buttons, grids, text blocks), along with the visual and logical trees, the dependency-property system, styles, triggers, and animations. None of those controls are Bevy entities; there is no entity per `<Button>`.
- **One entity per view.** The only ECS-visible part is the `NoesisView` camera entity. It renders one XAML document into the frame, and the whole tree behind it is opaque to the ECS.
- **You reach the controls through bridges, not entities.** Instead of querying widget entities, you attach bridge components to the view entity, and reconcile systems in `NoesisSet::Apply` push values into and pull them out of the live scene: `NoesisText` for text, `NoesisDp` for dependency properties, `NoesisVm` for view models, `NoesisCommands` for commands, and so on. Read-backs arrive as `Message { view, .. }` events, not as changed components on a widget. Data binding, commands, and view models are the seam, the idiomatic XAML/WPF approach rather than immediate-mode or an entity per widget.
- **The tradeoff.** You don't get ECS-native features like per-widget `Transform`, picking, or change detection for free. Anything you want to drive from gameplay goes through the bridge layer, or means adding to it.

On top of this base, the entity-driven API below raises whole panels and list rows to first-class entities when you want plain ECS ergonomics; the bridges stay the lower-level seam beneath it.

## The entity-driven UI API

When you'd rather drive the UI as plain ECS, three primitives make a Bevy entity the unit of UI. You still author the XAML; you spawn entities and write ordinary systems instead of string-keyed bridges. `examples/ecs_ui.rs` runs all three end to end.

**Panel = entity.** A `UiPanel` mounts a sub-XAML fragment into a host slot, and the entity's bound components are its `DataContext`:

```rust
#[derive(Component, NoesisViewModel, Clone, Copy)]
struct Health(f32);   // binds {Binding Health} inside the fragment

commands.spawn((UiPanel::new("hud.xaml").mount_into(view, "Slot"), Health(100.0)));

fn regen(mut q: Query<&mut Health, With<UiPanel>>) {
    for mut h in &mut q { h.0 = (h.0 + 1.0).min(100.0); }
}
```

Two panels of the same type bind independently. Show and hide is a `String` field bound to `Visibility` (`visibility::{VISIBLE, COLLAPSED, HIDDEN}`), with no converter.

**List = query.** A list's rows are entities. Tag an entity into a list and it appears as a row; the bound `ObservableCollection` is reconciled by `Entity`, so mutating one component updates only its row and selection survives a reorder:

```rust
#[derive(Component, NoesisViewModel, Clone)]
struct Item { name: String, qty: i32 }

commands.entity(view).insert(UiList::new("Inventory"));
commands.spawn((Item { name: "Potion".into(), qty: 3 }, ListedIn(view)));
```

The selected row carries a `Selected` marker, read back with `Query<&Item, With<Selected>>`.

**Events = observers.** UI events arrive as `EntityEvent`s targeting the entity they came from, a panel or a row:

```rust
fn use_item(on: On<UiClicked>, items: Query<&Item>) {
    if let Ok(item) = items.get(on.event_target()) {
        // the row the click came from
    }
}
```

## Driving the UI from systems

Each piece of UI state (text, visibility, list contents, and more) is a bridge component on the view entity. Spawning a `NoesisView` auto-attaches every per-view bridge as a Bevy required component, each defaulting to empty at no cost, so you write to them without spawning them by hand. A write set from `Startup` or `OnEnter`, before the scene exists, lands once the scene builds. The two binding bridges `NoesisVm` and `NoesisCommands` are the exception: add those yourself, since they need a class or command name.

For an app with a single view, `NoesisUi` finds it for you so a system doesn't spell out the query:

```rust
use bevy::prelude::*;
use noesis_bevy::{NoesisText, NoesisUi};

fn update_score(score: Res<Score>, mut ui: NoesisUi<&mut NoesisText>) {
    if !score.is_changed() { return; }
    let Some(mut text) = ui.get_mut() else { return };
    text.write("Score", score.0.to_string());
}
```

`NoesisUi<&mut T>` reads or writes a bridge component `T` on the single view; plain `NoesisUi` yields the view entity via `ui.entity()`, which matches the `view: Entity` that read-back messages carry. Its accessors return `None` (rather than skipping the system) when there isn't exactly one view, so a multi-view app routes by that entity instead.

## Custom controls and markup extensions

Write a control or a `{Binding}`-style markup extension in Rust, register it from a `Startup` system, and XAML can use it by name:

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

For finer control, three lower-level bridges sit underneath: `NoesisVm` (a view model built one property at a time), `NoesisItems` (fill a list or dropdown from a Rust collection), and `NoesisDp` (get, set, or watch any property on a named element directly, no binding required).

## Version compatibility

| Bevy | noesis_bevy |
|------|-------------|
| 0.19 | 0.13        |
| 0.18 | 0.10 – 0.12 |

`noesis_bevy` tracks Bevy: each Bevy minor gets a fresh `noesis_bevy` minor, and
patch releases stay on the row's Bevy version. Pin `wgpu` to the same version
Bevy's renderer uses (the crate does this for you) so render-device types stay
interchangeable.

## Setup

```sh
unzip NoesisGUI-NativeSDK-linux-3.2.13-Indie.zip -d ~/sdks/noesis-3.2.13
export NOESIS_SDK_DIR=~/sdks/noesis-3.2.13
export LD_LIBRARY_PATH=$NOESIS_SDK_DIR/Bin/linux_x86_64:$LD_LIBRARY_PATH
```

Symlink the SDK's font and data directories so the examples and the `NOESIS_VIEWER_THEME` loader can find them (these `assets/` paths are gitignored):

```sh
ln -sfn $NOESIS_SDK_DIR/Data/Fonts assets/Fonts
ln -sfn $NOESIS_SDK_DIR/Data        assets/Data
```

Apply your license credentials so the runtime runs licensed:

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
