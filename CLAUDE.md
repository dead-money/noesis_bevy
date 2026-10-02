# noesis_bevy

Bevy plugin that drives the closed-source Noesis GUI SDK — via the sibling FFI crate
`noesis_runtime` — and renders its UIs into a Bevy frame through `noesis_wgpu`'s
wgpu-backed `Noesis::RenderDevice`.

## Invariants

- **Two-crate split.** All `unsafe` lives in the sibling `../noesis_runtime` (the FFI
  quarantine); this crate is `unsafe_code = forbid`. Most new capabilities start there:
  expose the Noesis primitive in the runtime's C shim + Rust FFI, then add the Bevy glue
  here. `noesis_runtime` is ours and freely editable.
- **The render device is `noesis_wgpu`.** Device and shader work happens in the sibling
  `../noesis_wgpu`, also ours. This crate uses its wgpu 29 line, `release/0.1`; `main`
  there is on a newer wgpu. Patch to a local checkout of that branch to develop against it.
- **SDK never in the repo.** It lives at `$NOESIS_SDK_DIR` (per-developer licensed); never
  commit any SDK content. `build.rs` panics when the env var is unset.

## Attribution

Do not add `Co-Authored-By: Claude` trailers to commits or "Generated with Claude Code"
footers to PR bodies. Author lines and PR bodies stay clean.

## Pointers

- Open work: [`TODO.md`](./TODO.md).
- **Bridge pattern** — a feature is a `#[derive(Component)]` on the `NoesisView` camera
  entity + a reconcile system in `NoesisSet::Apply` that calls a `NoesisRenderState`
  `apply_*_for(entity, …)` / `poll_*_for(entity, …)` method against that view's live scene;
  read-backs surface as a `Message { view, … }`. Mirror an existing bridge module in `src/`
  (`text.rs`, `dp.rs`, `viewmodel.rs`, `commands.rs`, …) when adding a new one. An `x:Name`
  passed to any bridge may be scope-qualified (`"Host/Leaf"`) to reach an element inside a
  composed control's private namescope; the shared `resolve_named` in `render.rs` walks it.
- Try XAML live: `cargo run -p noesis_bevy --example xaml_viewer <path>`.
