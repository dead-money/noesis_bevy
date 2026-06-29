# TODO — dm_noesis_bevy

Open work **owned by this crate**: the wgpu render device, the render-graph compositing path,
and the Bevy integration surface. Roughly ordered by likelihood we'll want each, not a commitment
that everything here is planned. Completed work is recorded in git history, not here.

The sibling `noesis_runtime` (FFI quarantine) is at 0.10 and exposes almost the whole SDK surface,
so nearly all remaining bridge work is mechanical Bevy glue against an already-wrapped primitive.
The few things that genuinely need runtime/FFI work first are called out in §5.

---

## 1. Render device / shaders

- **Effects: `SHADOW` (50) / `BLUR` (51).** Need the `shadow` texture co-bound with `image`. With
  `downlevel_defaults` (4 bind groups, 0–3 full) `shadow` must share group(3) with `image`
  (texture+sampler at bindings 2/3), plus a second pixel uniform for `cbuffer1_ps`
  (group(1) binding(1)). If the live Bevy wgpu device reports `max_bind_groups > 4`, raising the
  device limit is the simpler path. Until these land, drop-shadow / blur scenes panic.
- **Effects: `CUSTOM_EFFECT` (52).** Needs user pixel-shader compilation via `Batch.pixelShader`.
- **`SDF_LCD_SOLID`.** Subpixel text needs dual-source blending (`@blend_src(1)` fragment output).
  Separate kickoff from `SDF_SOLID`.

## 2. Compositing & perf

- ~~**PPAA premultiplied blit.**~~ Done. Both compositing nodes now use a single premultiplied-alpha
  "over" blend (`PREMULTIPLIED_OVER`) with `LoadOp::Load`, so `RenderFlag::Ppaa`'s fractional-alpha
  edges composite correctly over the camera's clear colour instead of overwriting it (the old Core2d
  1:1 overwrite let the clear colour bleed through). Verified by `tests/wgpu_ppaa_blit.rs`.

## 3. Bevy integration — capability bridges (runtime-ready; pure Bevy glue)

Each rides the established per-element-component / app-plugin pattern; the runtime already wraps the
primitive (named below), so this is reconcile-system + test + sample work only.

- **Deeper styling.** `Style` `BasedOn` (`styles::Style::set_based_on`); `DataTrigger` /
  `MultiTrigger` / `EventTrigger` (`styles::{DataTrigger,MultiTrigger,EventTrigger}` — only property
  `Trigger` setters are wired today); `ControlTemplate` / `DataTemplate` parse + assign
  (`styles::ControlTemplate::parse`, `DataTemplate`).
- **`ResourceDictionary`.** Build / `parse` / `add` / `find` / `contains` / `add_merged` and the
  process-global application-resources inspect/install (`resources` module) — beyond the current
  `install_app_resources_chain`.
- **`MatrixTransform3D`.** The `CompositeTransform3D` bridge is in; the matrix form
  (`transforms::MatrixTransform3D`) isn't surfaced yet.
- **Richer geometry / shapes.** A shapes object model (`shapes` module: Rectangle/Ellipse/Line/…)
  beyond the current polyline `Path` (`geometry`).
- **`InlineUIContainer` + per-inline `TextDecorations`.** Both wrapped
  (`text_inlines::{InlineUIContainer, TextDecorations}`); not yet in the `NoesisInlines` `InlineSpec`.

## 3b. Bevy integration — other

- **Bevy-idiomatic SDK examples.** Re-implement a selection of the Noesis SDK samples
  (`$NOESIS_SDK_DIR/Data/`) as in-crate `examples/` driven through the bridge components — visible
  runnable demos that also exercise the bridges end-to-end.
- **Phase 5 corpus styling.** `assets/phase5/` Buttons set `Background`/`Foreground` without a
  `ControlTemplate`, so even themed they show the magenta no-Template placeholder. Fix by `BasedOn`
  a theme Style (now that the styles bridge exists) or dropping the custom Style.

## 4. Platform

- **Windows.** `build.rs` is Linux-only. Needs MSVC `Noesis.lib` import-library handling and DLL
  discovery/copy. Shared concern with the runtime crate's `build.rs`; coordinate the two.

## 5. Runtime gaps to fill first (then add the Bevy glue)

- **`InlineCollection::Clear`.** The `NoesisInlines` bridge can only populate a TextBlock whose
  `Inlines` is empty; re-applying changed inline content needs a clear wrapper in `noesis_runtime`
  (no `Clear` on `InlineCollection` in 0.10). Until then the bridge warns and skips a differing
  later spec.

---

### Notes on prioritization

Highest-leverage remaining item: **effects `SHADOW`/`BLUR` (§1)** — the last gate on the
shadow/blur slice of the SDK sample corpus.
