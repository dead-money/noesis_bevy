# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). While the crate is
pre-1.0, any `0.x` release may contain breaking changes.

## [Unreleased]

## [0.12.0] - 2026-07-02

### Added

- `NoesisHeadlessPlugin`: the full main-world bridge pipeline against a
  directly-requested wgpu device, with no render graph. Built as the test/CI
  harness, useful anywhere UI logic should run without Bevy's renderer.

### Changed

- The integration suite consolidates 82 one-test binaries into three suite
  binaries run under cargo-nextest (process-per-test; Noesis state is
  process-global and thread-affine — see `tests/README.md`), stepping frames
  event-driven via `run_until` instead of timer loops. Deterministic on real
  GPU runners: no more teardown segfaults/aborts after passing tests.

### Fixed

- The ten P0 findings from the full-crate audit (#62, landed in #63): per-phase
  vertex/index geometry streaming in the wgpu render device (multi-tile frames
  no longer read the last segment's bytes), zero-size views no longer abort on
  a zero-extent texture, list row types no longer clobber sibling lists, key
  auto-repeat reaches Noesis controls, focus one-shots no longer replay their
  history, animations inserted before the scene build apply, multi-view input
  routes deterministically with the target entity carried on the event, stale
  intermediates no longer ghost after teardown, bridge component removal reaps
  render-side state, and DataContext attach collisions warn.
- The sixteen P1 findings (#62, landed in #64): hot-reload keeps the last good
  scene until readiness gates re-pass, the default theme applies to
  late-spawned views, theme chains merge with code-built resources instead of
  clobbering them, list selection no longer emits phantom events, re-inserted
  bridge defs rebuild instead of freezing on first sight, plain view-models
  work on multiple views, idle frames no longer dirty change detection,
  panel-mount gates cover all panel bridges, panel teardown prunes its dedupe
  maps, dropped assets leave the registries, per-name items application stops
  cross-list resets, wheel events dispatch once, pointer-over-UI resets on
  teardown and cursor-leave, multi-window events filter to the primary window,
  the render device warns-and-defaults instead of panicking mid-frame, and a
  dozen smaller confirmed bugs.
- Cross-leaf `{StaticResource}` references in `application_resources` chains
  now resolve in dependency order in every configuration, including alongside
  code-built `NoesisResources` entries (#66, via `noesis_runtime` 0.12's
  `ResourceDictionary::set_source`).
- Removing `NoesisBinding` from a live view now unbinds its targets
  (`noesis_runtime` 0.12's `clear_binding`); `NoesisDiagnostics` gains
  `live_binding_count` (#66).

### Changed

- Requires `noesis_runtime` 0.12 (#66).
- The dead render-world extract plumbing is gone and the module docs describe
  the real threading model: the driving pipeline runs in main-world
  `PostUpdate` on the one thread the Noesis FFI is pinned to, and the painted
  intermediate is the only data that crosses to the render sub-app (#65).

## [0.11.2] - 2026-06-30

### Fixed

- **Unset stencil reference on clip clears.** Noesis's Clear stencil mode built a
  pipeline wgpu treats as stencil-enabled (so it enables the dynamic stencil
  reference) but whose `compare: Always` leaves `needs_ref_value()` false, so wgpu
  silently dropped every `set_stencil_reference`. Every clip-stencil clear then drew
  with an unset dynamic reference, tripping `VUID-vkCmdDrawIndexed-None-07839` —
  undefined behaviour that can escalate to a GPU fault on some drivers. Clear mode
  now carries a `fail_op: Replace` (dead under `compare: Always`) to keep the
  reference emitted.

## [0.11.1] - 2026-06-30

### Added

- **`NoesisPointerOverUi` resource.** A main-world flag, refreshed each frame from
  the view's pointer hit-test, so consumers can gate 3D-world picking on `!over`
  and stop clicks falling through hit-test-visible panels.

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
  empty slot.

- **Loud lenient-parse fragment failures.** A malformed-but-loadable `UiPanel`
  fragment (a tag mismatch loads as a partial tree with only a Noesis parser
  warning) now also logs a Bevy `error!` naming the panel entity, URI, and the
  warning.

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

[Unreleased]: https://github.com/dead-money/noesis_bevy/compare/v0.11.2...HEAD
[0.11.2]: https://github.com/dead-money/noesis_bevy/compare/v0.11.1...v0.11.2
[0.11.1]: https://github.com/dead-money/noesis_bevy/compare/v0.11.0...v0.11.1
[0.11.0]: https://github.com/dead-money/noesis_bevy/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/dead-money/noesis_bevy/releases/tag/v0.10.0
