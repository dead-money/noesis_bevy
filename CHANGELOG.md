# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). While the crate is
pre-1.0, any `0.x` release may contain breaking changes.

## [Unreleased]

## [0.11.0] - 2026-06-29

### Added

- **Scope-qualified element names.** Every bridge that targets an element by
  `x:Name` (text, dependency properties, visibility, geometry, transforms,
  brushes, styles, classes, focus, items, command/view-model `DataContext`
  attach, binding targets and `ElementName` sources, …) now accepts a
  `/`-separated path such as `"MainMenu/PlayButton"`. Each segment but the last
  names a composed control to descend into; the final segment resolves inside
  that control's own namescope. This lifts the long-standing limitation that a
  root-level `FindName` cannot see the names declared inside a composed
  `UserControl` (each such control owns a private namescope). Plain, unqualified
  names are unchanged and resolve exactly as before. Read-backs echo the original
  qualified string you supplied, so two controls that each contain an
  `"OkButton"` stay distinguishable.

- **Entity-driven UI API.** A Bevy entity is the unit of UI, across three
  primitives: `UiPanel` (its bound `#[derive(NoesisViewModel)]` components are the
  panel's `DataContext`), a query-backed `UiList` (rows are entities; the bound
  `ObservableCollection` is reconciled by `Entity` with minimal
  Add/Remove/Move/Update, and selection round-trips as a `Selected` marker), and
  UI events delivered as `EntityEvent`s (`On<UiClicked>`) targeting the
  originating entity. Runnable end to end in `examples/ecs_ui.rs`. Includes
  reactive teardown of despawned views and panels (releasing their Noesis state)
  and `ffi_hops` / apply-time diagnostics.

- **Panel-entity input watches.** A `NoesisClickWatch` / `NoesisKeyDownWatch` on a
  `UiPanel` entity resolves `x:Name`s inside that panel's own fragment namescope
  (a host-view lookup can't see them) and fires `UiClicked` / `UiKeyDown` targeting
  the panel entity, with the host as `view`. Buttons and keyed input inside mounted
  panel fragments now work without routing through the host scene.

- **Panel-entity write bridges.** `NoesisGeometry`, `NoesisLayout`, `NoesisFocus`,
  `NoesisFocusControl`, and `NoesisTransform` on a `UiPanel` entity now resolve
  `x:Name`s against the panel's own fragment namescope (like the input watches
  above), instead of silently no-op'ing because the entity isn't a view. This lets
  geometry / layout / focus / transform panels (a trace, a context-menu cursor, a
  console's focus) be split into their own mounted fragments rather than staying
  inline in the host scene.

- **Deferred panel seal.** `UiPanel::deferred_seal()` plus the `SealPanel` marker
  let a panel whose bound components are contributed by *separate modules* across
  frames freeze its `DataContext` on demand rather than on first sight, so a
  late-added field isn't dropped. The default (freeze on first bound component)
  and `static_context()` (freeze empty) are unchanged; the recommended pattern
  stays "one owning component holds all the fields."

- **Loud fragment load failures.** A `UiPanel` fragment whose URI can't be loaded
  (`FrameworkElement::load` returns `None` — an unregistered/typo'd URI, or XAML
  Noesis rejects outright) now logs a Bevy `error!` naming the panel entity and
  URI, instead of silently rendering an empty slot. Deduped per `(entity, uri)`;
  the panel degrades gracefully without crashing the app or blocking its siblings.
  (Noesis tolerates many *malformed* fragments by returning a partial tree with
  only its own parser warning; those don't reach this error path.)

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
