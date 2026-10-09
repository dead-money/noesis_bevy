# TODO

Open work on the wgpu render device, the compositing path, and the Bevy integration surface, roughly
in order of likely need. Render-device items (§1) land in `noesis_wgpu`, then reach this crate through
its 0.2 / wgpu 30 line or a `noesis_wgpu` bump. `noesis_runtime` wraps almost the whole SDK, so most bridge work is Bevy glue
over an existing primitive; items that need runtime work first are in §4.

## 1. Render

- **Subpixel LCD text (`SDF_LCD`).** Negotiate the wgpu `DUAL_SOURCE_BLENDING` feature per-device and set
  `caps().subpixel_rendering` only when it's available; cover the full LCD shader matrix
  (Solid / Linear / Radial / Pattern_*); validate subpixel coverage against real text.
- **`CUSTOM_EFFECT` (52).** A per-effect pipeline build keyed on `Batch.pixelShader` plus a WGSL
  authoring/transpile path for user shaders. Needs the runtime to surface the shader (§4).

## 2. Bevy

- **Styling triggers & templates.** `EventTrigger` (Storyboard / `BeginStoryboard` actions);
  `ControlTemplate` / `DataTemplate` parse + assign.
- **More SDK conformance examples.** Port more Noesis samples as in-crate `examples/`, loading the real
  sample XAML and assets from `$NOESIS_SDK_DIR` and driving data through the bridges, to show rendering
  and behavior match the reference.
- **Hot reload of an already-used font.** Adding a new font file reloads; editing the bytes of a face
  already in use doesn't, because Noesis's `CachedFontProvider` ignores a duplicate `RegisterFont` and
  has no cache reset. Fixing it means re-creating the process-global font provider and rebuilding every
  view, likely with a `noesis_runtime` font-cache-reset shim.

## 3. Platform

- **Windows CI.** Windows builds work (the runtime links MSVC `Noesis.lib`; `build.rs` copies
  `Noesis.dll` next to the test and example binaries) but CI doesn't run them. Verify with
  `cargo nextest run` on Windows with the SDK, and add a Windows runner to gate it.

## 4. Runtime

- **`Batch.pixelShader` exposure** for `CUSTOM_EFFECT`. The shim doesn't surface a custom effect's
  pixel-shader source or bytecode.
- **`NsApp::Window` root.** The App-framework `Window` type isn't linked, so samples whose XAML root is
  `<Window>` can't instantiate a real window. Link/wrap `NsApp::Window` to render `<Window>`-rooted
  samples faithfully.
- **Interactivity / behaviors.** The `NsApp` Interactivity package (`b:Interaction.Triggers`, behaviors
  like `EventTrigger` + `ControlStoryboardAction` / `SetFocusAction`) isn't registered, so
  behavior-driven sample interactions are inert. Wrap the Interactivity types in the shim.
