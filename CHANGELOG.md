# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). While the crate is
pre-1.0, any `0.x` release may contain breaking changes.

## [Unreleased]

## [0.15.1] - 2026-07-11

### Fixed

- **Adding an image to `ImageRegistry` no longer rebuilds every live scene.**
  The image hot-reload epoch bumped on any registry change — including a newly
  *added* URI and a change in entry count — so an app that stages procedural
  images at runtime (e.g. palette-preview thumbnails trickling in one per frame)
  rebuilt every view, every frame. A newly added URI cannot affect an
  already-built scene (nothing referenced it at build time), and a removed URI
  stays cached in Noesis, so only an in-place byte replacement of an
  already-present URI now bumps the epoch and forces the rebuild that genuine
  image hot-reload needs. Regression test:
  `headless_app_hot_reload_image_add`.

## [0.15.0] - 2026-07-11

### Added

- **`hot_reload` feature: live-edit the UI.** An opt-in, dev-only cargo feature
  (off by default) that reloads the running UI when its source files change on
  disk. It watches the files behind `XamlRegistry` with the `notify` crate (for
  XAML loaded off the `assets/` root via `XamlRegistry::insert`) and enables
  Bevy's `file_watcher` (for `AssetServer`-loaded XAML / images / fonts), then
  turns a byte change into a targeted view rebuild. Coverage:
  - a view's **root XAML**;
  - transitive **`Source=` dictionaries** merged into a view or fragment — a
    provider fetch-log records what each build pulled, so editing a shared
    `ResourceDictionary` reloads every view that merged it;
  - mounted **`UiPanel` fragments**, which re-parse and re-mount on edit;
  - **images**, which re-size and repaint (the rebuild re-issues Noesis's
    per-URI `LoadTexture`, which it otherwise caches);
  - **application-resources / theme dictionaries** referenced through
    `NoesisView.application_resources`, which reinstall and restyle live views.

  Register files to watch with `NoesisHotReload::watch(uri, path)`; the
  `xaml_viewer` example wires it up (run with `--features hot_reload`). Same-file
  **font** reload is not yet supported — Noesis caches font faces process-globally
  — though adding a *new* font file still works.

## [0.14.1] - 2026-07-05

### Added

- `NoesisClip`: a per-view clip bridge that assigns an imperative polygon clip to
  any named element (points in the element's own space; an empty polygon clears
  the clip), applied on change like the transform and geometry bridges. Rewriting
  the polygon each frame animates a moving clip region. Needs `noesis_runtime`
  0.12.1 (`set_clip_points`).

## [0.14.0] - 2026-07-02

### Changed

- **`UiList` is now its own entity (breaking).** A list no longer rides on the
  `NoesisView`; it is a distinct entity that names the view it renders into, so a
  single view can bind any number of `ListBox`es. Spawn `UiList::new(view, "Name")`
  on its own entity and attach rows to *that* entity via `ListedIn(list)`. Migrate:

  ```rust
  // before
  commands.entity(view).insert(UiList::new("Inventory"));
  commands.spawn((Item { /* … */ }, ListedIn(view)));

  // after
  let list = commands.spawn(UiList::new(view, "Inventory")).id();
  commands.spawn((Item { /* … */ }, ListedIn(list)));
  ```

  `ListedIn` is now a Bevy relationship (its `ListRows` target on the list entity
  tracks membership automatically), and rows appear in `ListedIn`-insertion order
  (for the usual append-as-you-spawn pattern, unchanged). A UI selection now also
  triggers a `NoesisRowSelected` `EntityEvent` on the selected row entity, alongside
  the existing buffered `NoesisListSelection` message.

## [0.13.0] - 2026-07-02

### Changed

- **Bevy 0.19 / wgpu 29.** The crate tracks Bevy 0.19 (see the README's version
  map; Bevy 0.18 users stay on 0.12). The UI composite now runs as plain
  systems in Bevy's `Core2d`/`Core3d` schedules (the render graph is gone in
  0.19): after the 2D main pass and before tonemapping, and for tagged
  `NoesisCamera` 3D views after post-processing and before upscaling.
  Extracted components carry an explicit `SyncComponent` contract.

### Fixed

- The spirv-val `AtomicIAdd` validation spam on Vulkan is gone (upstream naga
  fix shipped with wgpu 28+, inherited via wgpu 29).

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

[Unreleased]: https://github.com/dead-money/noesis_bevy/compare/v0.15.0...HEAD
[0.15.0]: https://github.com/dead-money/noesis_bevy/compare/v0.14.1...v0.15.0
[0.14.1]: https://github.com/dead-money/noesis_bevy/compare/v0.14.0...v0.14.1
[0.11.2]: https://github.com/dead-money/noesis_bevy/compare/v0.11.1...v0.11.2
[0.11.1]: https://github.com/dead-money/noesis_bevy/compare/v0.11.0...v0.11.1
[0.11.0]: https://github.com/dead-money/noesis_bevy/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/dead-money/noesis_bevy/releases/tag/v0.10.0
