# TODO — dm_noesis_bevy

Open work **owned by this crate**: the wgpu render device, the render-graph compositing path, and the
Bevy integration surface. Roughly ordered by likelihood we'll want each. Completed work is recorded in
git history, not here.

The sibling `noesis_runtime` (FFI quarantine) is at 0.10 and wraps almost the whole SDK surface, so
nearly all remaining bridge work is mechanical Bevy glue against an already-wrapped primitive. The few
things that need runtime/FFI work first are in §4.

---

## 1. Render device / shaders

- **SDF_LCD production enablement.** The `SDF_LCD_SOLID` shader + dual-source blend are in
  (`tests/wgpu_sdf_lcd.rs`, gated on the wgpu `DUAL_SOURCE_BLENDING` feature) but **not enabled in
  production** — `caps().subpixel_rendering` stays `false`. Remaining: (a) negotiate
  `DUAL_SOURCE_BLENDING` per-device in the render app and set the cap only when supported; (b) extend
  the LCD matrix beyond `SDF_LCD_SOLID` (Linear / Radial / Pattern_* variants); (c) validate subpixel
  coverage against real text.
- **`CUSTOM_EFFECT` (52).** User pixel-shader path via `Batch.pixelShader` — needs the runtime to
  surface the effect's shader (see §4), then a per-effect pipeline build keyed on `Batch.pixelShader`
  and a WGSL authoring/transpile path for user shaders.

## 2. Bevy integration

- **Styling: `EventTrigger` + templates.** `EventTrigger` (needs Storyboard / `BeginStoryboard`
  authoring); `ControlTemplate::parse` + `DataTemplate` assign (distinct apply paths from the Style
  setter/trigger bridge that already exists).
- **More SDK example ports.** `controls_gallery` and `databinding` are in; port additional
  representative Noesis samples (`$NOESIS_SDK_DIR/Data/`) as in-crate `examples/` driven through the
  bridge components — visible demos that also exercise the bridges.
- **Phase 5 corpus styling.** `assets/phase5/` Buttons set `Background`/`Foreground` without a
  `ControlTemplate`, so even themed they show the magenta no-Template placeholder. Fix by `BasedOn`
  a theme Style (the styles bridge now exists) or dropping the custom Style.

## 3. Platform

- **Windows.** `build.rs` is Linux-only. Needs MSVC `Noesis.lib` import-library handling and DLL
  discovery/copy. Shared concern with the runtime crate's `build.rs`; coordinate the two.

## 4. Runtime gaps to fill first (then add the Bevy glue)

- **`Batch.pixelShader` exposure** for `CUSTOM_EFFECT` — the shim doesn't surface a custom effect's
  pixel-shader source/bytecode yet.

---

### Notes on prioritization

Highest-leverage remaining: **SDF_LCD production enablement (§1)** for crisp subpixel text, and the
**styling templates (§2)** which unlock `ControlTemplate`-based control galleries.
