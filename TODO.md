# TODO — dm_noesis_bevy

Open work **owned by this crate**: the wgpu render device, the render-graph compositing path,
and the Bevy integration surface. Roughly ordered by how likely we are to want each one, not a
commitment that everything here is planned. (Completed work is recorded in git history, not here.)

Feature-*exposure* work is driven separately. When we want to surface a Noesis capability that
the C ABI doesn't wrap yet, the gap and ordering live in
[`../dm_noesis_runtime/TODO.md`](https://github.com/dead-money/dm_noesis_runtime); the job on
this side is then mechanical glue once the runtime exposes the primitive. So this list does
**not** re-enumerate the SDK surface; it covers only what we author ourselves.

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
- **Onscreen-path draw renders nothing (`tests/wgpu_first_triangle.rs`, `#[ignore]`d).** Driving
  the device's *onscreen* path manually — `set_onscreen_target` → `begin_onscreen_render` →
  `draw_batch(PATH_SOLID)` → `end_onscreen_render` — with a hand-built identity-projection
  triangle produces **zero** non-clear pixels (`cull_mode: None`, so not culling). Every
  *offscreen*-path device test passes (`wgpu_{offscreen_rt,pattern,radial,uniform_ring,
  multi_shader}`), so the regression is specific to the onscreen entry the test exercises.
  The onscreen path also backs `bake` and the live intermediate, so this is worth a real
  look. First step: confirm whether the vs projection uniform reaches the draw (a zeroed
  cbuffer collapses the triangle to the origin → nothing rasterised). Un-ignore the test when fixed.
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
  `NoesisView.ppaa` (viewer `P` key), off by default. Proper fix is a premultiplied blit,
  or opaque-with-pre-clear; lands when text/effects demand AA.
- **Direct-to-`ViewTarget`.** Today Noesis paints an intermediate `Rgba8Unorm` texture and
  `NoesisNode` blits it into the camera's `ViewTarget`. Keying the `PipelineCache` on color
  format and retargeting Noesis's pipelines at the `ViewTarget` format would let us drop the
  intermediate and the blit pass entirely. The main perf win on the table (per view).

## 3. Bevy integration

- **XAML hot-reload.** Rebuild the `View` on Bevy asset-reload events so editing a `.xaml`
  refreshes live. Pairs with the runtime's `ParseXaml` / `LoadComponent` work.
- **Remaining capability bridges.** The interaction core, visual richness (brushes, transforms,
  animation, imaging, svg), data/text (typed items, binding/converters, typography), and
  system surface (diagnostics, integration) are bridged. Still un-bridged, in the established
  per-element-component / app-plugin style: **styles / templates / triggers + `ResourceDictionary`**
  access (code-built styles beyond `install_app_resources_chain`); **formatted text / inlines /
  OpenType typography** attached properties; **3D transforms** (`CompositeTransform3D` /
  `MatrixTransform3D`); a richer **geometry / shapes** object model (beyond the polyline `Path`);
  and **collection-view** operations (sort/filter/group/navigation). Each rides the same pattern.
- **Phase 5 corpus styling.** `assets/phase5/` Buttons set `Background`/`Foreground` without
  a `ControlTemplate`, so even themed they show the magenta no-Template placeholder. Fix by
  `BasedOn` a theme Style or dropping the custom Style.

## 4. Platform

- **Windows.** `build.rs` is Linux-only. Needs MSVC `Noesis.lib` import-library handling and
  DLL discovery/copy. Shared concern with the runtime crate's `build.rs`; coordinate the two.

---

### Notes on prioritization

If we build nothing else, the two highest-leverage items are:

1. **The effects pipeline (§1)** — the difference between "renders our hand-authored scenes"
   and "renders the SDK sample corpus," and it gates a real chunk of XAML.
2. **Direct-to-`ViewTarget` (§2)** — removes a full-frame texture allocation and blit per view.

The stencil fix (§1) is smaller and unblocks themed `ScrollViewer`, which shows up in almost
every real control gallery, so it's a good early win despite the lower leverage.
