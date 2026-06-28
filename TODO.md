# TODO — dm_noesis_bevy

This tracks open work that is **owned by this crate**: the wgpu render device, the
render-graph compositing path, and the Bevy integration surface. It's roughly ordered
by how likely we are to want each one, not a commitment that everything here is planned.

Feature-*exposure* work is driven separately. When we want to surface a Noesis capability
that the C ABI doesn't wrap yet (data binding, generic property access, more routed events,
animation control, and so on), the gap and ordering live in
[`../dm_noesis_runtime/TODO.md`](https://github.com/dead-money/dm_noesis_runtime). The job
on this side is then mechanical: add the Bevy-facing component / resource / system once the
runtime exposes the primitive. So this list deliberately does **not** re-enumerate the SDK
surface; it covers only what the runtime crate can't see, because we author it ourselves.

## Already covered (for reference)

`NoesisPlugin` boot + clean shutdown; the wgpu `RenderDevice` impl with a pipeline cache
keyed on `(shader, render_state, vertex_format)`, a `#ifdef` WGSL preprocessor, and a
`UniformRing` for per-batch uniforms; the implemented shader set (`Path_Solid`,
`Path_AA_Solid`, `Mask`, `RGBA`, `Clear`, `PATH_PATTERN` +AA with all wrap variants,
`PATH_LINEAR` / `PATH_RADIAL` +AA, `SDF_SOLID`); the `BlendMode` matrix (Src / SrcOver /
SrcOverMultiply / SrcOverScreen / SrcOverAdditive); the intermediate-texture + `NoesisNode`
blit with sRGB round-trip; XAML / font / image asset loaders and providers; the
main→render extraction of scene + registries; the input plugin (pointer / keyboard / char /
wheel / touch / focus); the routed `Click` bridge; and the custom class / markup-extension
lifecycles.

---

## 1. Render device / shaders

The render device is entirely ours, so every shader gap is this crate's work. This is the
largest open area.

- **Effects pipeline.** Opacity / Shadow / Blur / Downsample / Upsample are unimplemented;
  scenes that need them (`Transform3D.xaml`, `Effects.xaml`) panic on
  `Shader(49)=DOWNSAMPLE`. This also covers the offscreen render-target effect path (blur,
  shadow, opacity groups, custom `ShaderEffect` via `Batch.pixelShader`).
- **`SDF_LCD_SOLID`.** Subpixel text needs dual-source blending (`@blend_src(1)` fragment
  output). Separate kickoff from `SDF_SOLID`.
- **Stencil not attached.** `create_render_target` allocates a stencil texture but no
  pipeline declares `depth_stencil`. Suspected cause of the **ScrollViewer content-viewport
  blank under theme** bug (`03_scroll.xaml` + `NOESIS_VIEWER_THEME=DarkBlue`: scrollbar
  chrome renders but the content interior is flat white and spills its clip). Next step: log
  `draw_batch` for one frame and diff against the `scrollviewer-no-theme` baseline in
  `tests/headless_offscreen_brush.rs`. Other suspect: `LoadOp::Load` on a fresh RT reading
  uninitialised memory.

## 2. Compositing & perf

- **PPAA + alpha blend.** `RenderFlag::Ppaa` produces fractional-alpha edges; with the
  blit's alpha-blending the camera clear color bleeds through. Toggleable via
  `NoesisScene.ppaa` (viewer `P` key), off by default. Proper fix is a premultiplied blit,
  or opaque-with-pre-clear; lands when text/effects demand AA.
- **Direct-to-`ViewTarget`.** Today Noesis paints an intermediate `Rgba8Unorm` texture and
  `NoesisNode` blits it into the camera's `ViewTarget`. Keying the `PipelineCache` on color
  format and retargeting Noesis's pipelines at the `ViewTarget` format would let us drop the
  intermediate and the blit pass entirely. The main perf win on the table.

## 3. Bevy integration

- **Multi-view.** Effectively one scene today. Support multiple `NoesisView` entities, each
  with its own `View`, intermediate, and target camera.
- **XAML hot-reload.** Rebuild the `View` on Bevy asset-reload events so editing a `.xaml`
  refreshes live. Pairs with the runtime's `ParseXaml` / `LoadComponent` work.
- **Component-vs-resource API.** `NoesisScene` is a single resource. A component-based scene
  (one entity per view, scene config as a component) is the idiomatic Bevy shape and a
  prerequisite for multi-view.
- **Bevy surface for newly-wrapped runtime features.** As the runtime exposes primitives
  (`VisualStateManager::GoToState`, more routed events, …), add the matching Bevy ergonomics:
  typed components, `Reflect` integration where it fits, and event bridges in the style of the
  existing `NoesisClicked`. Driven by `../dm_noesis_runtime/TODO.md`; no separate enumeration
  here. Landed so far:
  - **Data binding / ViewModel bridge** (`src/viewmodel.rs`, `NoesisViewModelPlugin`) — declare
    a Rust-owned view model (`ViewModelDef`), attach it as a `DataContext` render-side, write its
    DPs from app code, and receive two-way control edits as `NoesisViewModelChanged` messages.
    Needed the safe `FrameworkElement::set_data_context(&ClassInstance)` accessor added to the
    runtime (`unsafe_code = forbid` here can't call the raw pointer variant).
  - **`ItemsSource` bridge** (`src/items.rs`, `NoesisItemsPlugin`) — populate list controls
    (`ComboBox` / `ListBox`) from Rust: `NoesisItemsSources::set`/`push`/`remove_at`/`clear` drive
    a per-`x:Name` `ObservableCollection` bound via the safe
    `set_items_source(&ObservableCollection)`. String items only (the safe collection surface is
    `push_string`); typed items would need a safe `push_*` added to the runtime.
  - *Remaining:* a generic DP get/set bridge keyed by `(x:Name, property)` (a near-mechanical
    mirror of `text.rs`, independently useful for binding-free control access).
- **Phase 5 corpus styling.** `assets/phase5/` Buttons set `Background`/`Foreground` without
  a `ControlTemplate`, so even themed they show the magenta no-Template placeholder. Fix by
  `BasedOn` a theme Style or dropping the custom Style.

## 4. Platform

- **Windows.** `build.rs` is Linux-only. Needs MSVC `Noesis.lib` import-library handling and
  DLL discovery/copy. Shared concern with the runtime crate's `build.rs`; coordinate the two.

---

### Notes on prioritization

If we build nothing else, the two highest-leverage items are:

1. **The effects pipeline (§1)** — it's the difference between "renders our hand-authored
   scenes" and "renders the SDK sample corpus," and it gates a real chunk of XAML.
2. **Direct-to-`ViewTarget` (§2)** — removes a full-frame texture allocation and blit per
   view, and is a prerequisite for multi-view not multiplying that cost.

The stencil fix (§1) is smaller and unblocks themed `ScrollViewer`, which shows up in almost
every real control gallery, so it's a good early win despite the lower leverage.
