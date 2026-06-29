# TODO — dm_noesis_bevy

Open work owned by this crate: the wgpu render device, the render-graph compositing path, and the Bevy
integration surface. Roughly ordered by likelihood we'll want each. Completed work is recorded in git
history, not here.

The sibling `noesis_runtime` (FFI quarantine) wraps almost the whole SDK surface, so most remaining
bridge work is mechanical Bevy glue against an already-wrapped primitive. The things that need
runtime/FFI work first are in §4.

---

## 1. Render

- **Subpixel LCD text (`SDF_LCD`).** Negotiate the wgpu `DUAL_SOURCE_BLENDING` feature per-device and set
  `caps().subpixel_rendering` only when it's available; cover the full LCD shader matrix
  (Solid / Linear / Radial / Pattern_*); validate subpixel coverage against real text.
- **`CUSTOM_EFFECT` (52).** A per-effect pipeline build keyed on `Batch.pixelShader` plus a WGSL
  authoring/transpile path for user shaders (needs the runtime to surface the shader — see §4).

## 2. Bevy

- **Styling triggers & templates.** `EventTrigger` (Storyboard / `BeginStoryboard` actions);
  `ControlTemplate` / `DataTemplate` parse + assign.
- **More SDK conformance examples.** Port additional Noesis samples as faithful in-crate `examples/` —
  the real sample XAML/assets loaded from `$NOESIS_SDK_DIR` at runtime, data driven through the bridge
  components — to demonstrate our rendering/behavior matches the reference.

## 3. Platform

- **Windows.** `build.rs` is Linux-only. Needs MSVC `Noesis.lib` import-library handling and DLL
  discovery/copy. Shared concern with the runtime crate's `build.rs`; coordinate the two.

## 4. Runtime

- **`Batch.pixelShader` exposure** for `CUSTOM_EFFECT` — the shim doesn't surface a custom effect's
  pixel-shader source/bytecode.
- **`NsApp::Window` root.** The App-framework `Window` type isn't linked, so samples whose XAML root is
  `<Window>` can't instantiate a real window. Link/wrap `NsApp::Window` to render `<Window>`-rooted
  samples faithfully.
- **Interactivity / behaviors.** The `NsApp` Interactivity package (`b:Interaction.Triggers`, behaviors
  like `EventTrigger` + `ControlStoryboardAction` / `SetFocusAction`) isn't registered, so
  behavior-driven sample interactions are inert. Wrap the Interactivity types in the shim.
