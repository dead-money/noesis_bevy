# TODO — dm_noesis_bevy

Open work **owned by this crate**: the wgpu render device, the render-graph compositing path,
and the Bevy integration surface. Roughly ordered by likelihood we'll want each, not a commitment
that everything here is planned. Completed work is recorded in git history, not here.

The sibling `noesis_runtime` (FFI quarantine) is at 0.10 and exposes almost the whole SDK surface,
so nearly all remaining bridge work is mechanical Bevy glue against an already-wrapped primitive.
The few things that genuinely need runtime/FFI work first are called out in §5.

---

## 1. Render device / shaders

- **Effects: `SHADOW` (50) / `BLUR` (51).** *Done.* The `shadow` texture is co-bound with `image`
  on group(3) (texture+sampler at bindings 2/3) and `cbuffer1_ps` rides group(1) binding(1), staying
  within the 4-bind-group `downlevel_defaults` limit (the more portable of the two options in the
  original plan). Covered by `tests/wgpu_shadow_blur.rs`.
- **`SDF_LCD_SOLID`.** *Implemented* (shader + `SrcOver_Dual` blend + device plumbing), covered by
  `tests/wgpu_sdf_lcd.rs` (gated on the wgpu `DUAL_SOURCE_BLENDING` feature). **Not yet enabled in
  production**: `caps()` still reports `subpixel_rendering = false`, because flipping it makes Noesis
  emit the whole `SDF_LCD_*` matrix on every device — which needs per-device `DUAL_SOURCE_BLENDING`
  negotiation in the render app, and the SDK ships no LCD reference to validate the exact subpixel
  coverage against. Remaining: (a) wire the device feature so the cap is set only when supported,
  (b) extend the LCD matrix beyond `SDF_LCD_SOLID` (Linear/Radial/Pattern_*), (c) validate coverage
  against real text.
- **Effects: `CUSTOM_EFFECT` (52).** *Deferred.* Needs user pixel-shader compilation via
  `Batch.pixelShader`. The shim doesn't expose the effect's pixel-shader source/bytecode yet, and
  there's no WGSL transpile path for user HLSL — this needs runtime/FFI work first, then a per-effect
  pipeline build keyed on `Batch.pixelShader`.

## 2. Compositing & perf

- **PPAA premultiplied blit.** `RenderFlag::Ppaa` produces fractional-alpha edges; the blit's
  straight-alpha blend lets the camera clear color bleed through. Toggleable via `NoesisView.ppaa`
  (viewer `P` key), off by default. Fix the blit to composite premultiplied (or opaque-with-pre-clear).

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
