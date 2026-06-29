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

- **Effects pipeline (partial).** Opacity / Downsample / Upsample are implemented; the
  separable-blur resolve halves (`DOWNSAMPLE` 49, `UPSAMPLE` 48) and opacity groups render
  over the offscreen RT path (`tests/wgpu_effects.rs`). **Still open:** `SHADOW` (50) and
  `BLUR` (51) need the `shadow` texture co-bound with `image` — blocked by the 4-bind-group
  `downlevel_defaults` limit (groups 0–3 are full), so `shadow` must share group(3) with
  `image` (texture+sampler at bindings 2/3), plus a second pixel uniform for `cbuffer1_ps`
  (group(1) binding(1)). `CUSTOM_EFFECT` (52) needs user pixel-shader compilation via
  `Batch.pixelShader`. Scenes using drop-shadow/blur still panic until SHADOW/BLUR land.
- **`SDF_LCD_SOLID`.** Subpixel text needs dual-source blending (`@blend_src(1)` fragment
  output). Separate kickoff from `SDF_SOLID`.

(Resolved: the onscreen-path draw bug — the manual `wgpu_first_triangle` path was driven with
`RenderState::default()`, whose colorEnable bit is 0, so the pipeline used an empty color-write
mask; the test now passes with a proper render state. Stencil is now attached: render targets
and the onscreen intermediate carry a `Stencil8` buffer, pipelines declare a `depth_stencil`
state from the batch's stencil mode, and `draw_batch` clears + accumulates the clip stack —
`tests/wgpu_stencil_clip.rs`.)

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
