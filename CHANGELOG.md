# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). While the crate is
pre-1.0, any `0.x` release may contain breaking changes.

## [Unreleased]

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
