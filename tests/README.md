# Integration tests

Run these with [cargo-nextest](https://nexte.st), not `cargo test`:

```sh
cargo nextest run          # all suites
cargo nextest run -E 'binary(headless_suite)'   # one suite
```

## Why nextest is required

Noesis' class and resource registration is **process-global and thread-affine**.
Two Noesis-initializing tests in the same process is undefined behavior — the
nondeterministic teardown SIGSEGV documented in
[`render_suite/headless_bake_label.rs`](./render_suite/headless_bake_label.rs).

nextest runs every `#[test]` in its **own process**, which is exactly the
isolation these tests need. It is nextest's only execution model, so there is
nothing to configure beyond [`.config/nextest.toml`](../.config/nextest.toml)
(which caps parallelism so concurrent GPU device creation doesn't thrash the
driver).

`cargo test` runs all `#[test]`s of a binary in **one** process (parallel threads
by default). `--test-threads=1` does **not** help: it is still one process, just
serial. So a plain `cargo test` on these suites is unsafe. To fail loudly instead
of crashing, each Noesis-initializing test first calls `claim_noesis_process()`
(in [`common/mod.rs`](./common/mod.rs)): the first init in a process succeeds, the
second panics with a pointer back here. Under nextest each test is a fresh
process, so the interlock never trips.

## Layout

The ~82 former one-test-per-file binaries are consolidated into three suite
binaries (audit R2), each linking Bevy + Noesis once. Every file in a suite dir
is one `#[test]` module, declared in that suite's `main.rs`.

- **`headless_suite/`** — bridge tests on `MinimalPlugins` + `NoesisHeadlessPlugin`
  (no render graph, no pipeline compilation) plus the direct-Noesis unit tests.
  Driven by `common::run_until` (steps `app.update()`, no sleep).
- **`wgpu_suite/`** — direct-wgpu render tests and the Noesis-on-wgpu
  render-device tests. No Bevy app; each requests its own wgpu device.
- **`render_suite/`** — the few tests that need the real `DefaultPlugins` render
  graph. Driven by `run_until` then `common::settle` (drains in-flight pipeline
  compiles before the app drops).

Shared helpers live in [`common/mod.rs`](./common/mod.rs), path-included once per
suite `main.rs`.
