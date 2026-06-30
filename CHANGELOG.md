# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). While the crate is
pre-1.0, any `0.x` release may contain breaking changes.

## [Unreleased]

### Fixed

- **Focus / keydown into mounted fragments.** A `NoesisFocus` set on a `UiPanel`
  entity now re-applies once the panel's fragment mounts, instead of applying only
  on the frame the component was set (before the fragment existed) and never
  retrying. This makes keyed input into a focused fragment element work end to end.

### Added

- **Loud lenient-parse fragment failures.** A malformed-but-loadable `UiPanel`
  fragment (a tag mismatch loads as a partial tree with only a Noesis parser
  warning) now logs a Bevy `error!` naming the panel entity, URI, and the warning
  (with line/column), instead of a silent half-render. Complements the 0.11.0
  hard-load-failure error.

## [0.11.0] - 2026-06-29

### Added

- **Scope-qualified element names.** Bridges that target an element by `x:Name`
  now accept a `/`-separated path (`"MainMenu/PlayButton"`) to reach inside a
  composed control's private namescope. Plain names are unchanged; read-backs echo
  the qualified string.

- **Entity-driven UI API.** A Bevy entity is the unit of UI: `UiPanel` (its bound
  `#[derive(NoesisViewModel)]` components are its `DataContext`), a query-backed
  `UiList` (rows are entities, reconciled by `Entity`; selection round-trips as a
  `Selected` marker), and UI events as `EntityEvent`s (`On<UiClicked>`) targeting
  the entity. See `examples/ecs_ui.rs`. Adds despawn teardown and `ffi_hops` /
  apply-time diagnostics.

- **Panel-entity input watches.** `NoesisClickWatch` / `NoesisKeyDownWatch` on a
  `UiPanel` entity resolve `x:Name`s inside the panel's own fragment and fire
  `UiClicked` / `UiKeyDown` targeting the panel entity.

- **Panel-entity write bridges.** `NoesisGeometry`, `NoesisLayout`, `NoesisFocus`,
  `NoesisFocusControl`, and `NoesisTransform` on a `UiPanel` entity now resolve
  `x:Name`s inside the panel's fragment (like the input watches), so those panels
  can live in their own mounted fragments.

- **Deferred panel seal.** `UiPanel::deferred_seal()` and the `SealPanel` marker
  let a panel whose bound components come from several modules across frames freeze
  its `DataContext` on demand instead of on first sight. The default and
  `static_context()` are unchanged.

- **Loud fragment load failures.** A `UiPanel` fragment whose URI can't load now
  logs a deduped Bevy `error!` with the panel entity and URI, instead of a silent
  empty slot. (Malformed-but-loadable fragments still get only Noesis's own parser
  warning.)

- **`#[noesis(rename = "…")]`** field attribute: bind a snake_case field to a
  different XAML property name (`master_volume` → `{Binding MasterVolume}`).

- **`visibility::{VISIBLE, COLLAPSED, HIDDEN}`** consts for the show/hide pattern:
  bind a `String` field to `Visibility="{Binding …}"`; no `bool`-to-`Visibility`
  converter needed.

## [0.10.0] - 2026-06-29

First public release. A Bevy 0.18 plugin that renders Noesis XAML interfaces into
your frame: it runs Noesis on Bevy's GPU, composites the result onto a camera, and
drives the UI through per-view bridge components for text, visibility, data
binding, dependency properties, list contents, commands, focus, transforms, and
more, with read-backs delivered as messages. `NoesisUi` resolves the single view
in a one-UI app, and a `NoesisView` auto-attaches the bridges so a value set before
the scene exists lands once it builds. The version starts at 0.10.0 to move in step
with `noesis_runtime`.

[Unreleased]: https://github.com/dead-money/noesis_bevy/compare/v0.10.0...HEAD
[0.10.0]: https://github.com/dead-money/noesis_bevy/releases/tag/v0.10.0
