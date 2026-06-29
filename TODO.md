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

- **Effects: `SHADOW` (50) / `BLUR` (51).** Need the `shadow` texture co-bound with `image` —
  blocked by the 4-bind-group `downlevel_defaults` limit (groups 0–3 are full), so `shadow` must
  share group(3) with `image` (texture+sampler at bindings 2/3), plus a second pixel uniform for
  `cbuffer1_ps` (group(1) binding(1)). Until these land, drop-shadow / blur scenes panic.
- **Effects: `CUSTOM_EFFECT` (52).** Needs user pixel-shader compilation via `Batch.pixelShader`.
- **`SDF_LCD_SOLID`.** Subpixel text needs dual-source blending (`@blend_src(1)` fragment output).
  Separate kickoff from `SDF_SOLID`.

## 2. Compositing & perf

- **PPAA + alpha blend.** `RenderFlag::Ppaa` produces fractional-alpha edges; with the blit's
  alpha-blending the camera clear color bleeds through. Toggleable via `NoesisView.ppaa` (viewer
  `P` key), off by default. Proper fix is a premultiplied blit, or opaque-with-pre-clear.
- **Direct-to-`ViewTarget`.** Today Noesis paints an intermediate `Rgba8Unorm` texture and
  `NoesisNode` blits it into the camera's `ViewTarget`. Keying the `PipelineCache` on color format
  and retargeting Noesis's pipelines at the `ViewTarget` format would drop the intermediate and the
  blit pass entirely. The main perf win on the table (per view).

## 3. Bevy integration

- **Remaining capability bridges**, in the established per-element-component / app-plugin style:
  - **Deeper styling** — `Style` `BasedOn` inheritance; `DataTrigger` / `MultiTrigger` /
    `MultiDataTrigger` / `EventTrigger` (only property `Trigger` setters are wired);
    `ControlTemplate` / `DataTemplate` parse + assign.
  - **`ResourceDictionary` access** — get / add / merge / contains beyond
    `install_app_resources_chain`.
  - **Richer geometry / shapes** object model (beyond the polyline `Path`).
  - **`MatrixTransform3D`** (the `CompositeTransform3D` bridge is in; matrix form isn't).
  - **`InlineUIContainer`** (hosting a `UIElement` in flow content) + per-inline `TextDecorations`.
- **Bevy-idiomatic SDK examples.** Re-implement a selection of the Noesis SDK samples
  (`$NOESIS_SDK_DIR/Data/`) as in-crate `examples/` driven through the bridge components — visible
  runnable demos that also exercise the bridges end-to-end.
- **Phase 5 corpus styling.** `assets/phase5/` Buttons set `Background`/`Foreground` without a
  `ControlTemplate`, so even themed they show the magenta no-Template placeholder. Fix by
  `BasedOn` a theme Style or dropping the custom Style.

## 4. Platform

- **Windows.** `build.rs` is Linux-only. Needs MSVC `Noesis.lib` import-library handling and DLL
  discovery/copy. Shared concern with the runtime crate's `build.rs`; coordinate the two.

## 5. Runtime-blocked (file under `noesis_runtime` first, then add Bevy glue)

- **Inline content re-apply.** The inlines bridge only populates a TextBlock whose `Inlines` is
  empty; changing content later needs a scene rebuild. Lifting this needs an
  `InlineCollection::Clear` wrapper in the runtime FFI.
- **Collection-view sort / filter / group.** libNoesis 3.2.13 exposes no programmatic
  `SortDescription` collection or `Filter` delegate — only current-item navigation + `Refresh`
  (which the items bridge already drives). Needs a runtime surface first.

---

### Notes on prioritization

The highest-leverage remaining items: **effects `SHADOW`/`BLUR` (§1)** — the last gate on the
shadow/blur slice of the SDK corpus — and **direct-to-`ViewTarget` (§2)**, the main per-view perf
win.
