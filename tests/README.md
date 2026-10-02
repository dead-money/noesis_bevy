# Integration tests

Run these with [cargo-nextest](https://nexte.st), not `cargo test`:

```sh
cargo nextest run                               # all suites
cargo nextest run -E 'binary(headless_suite)'   # one suite
```

## Why nextest is required

Noesis' class and resource registration is process-global and thread-affine.
Two Noesis-initializing tests in one process is undefined behavior and crashes
nondeterministically at teardown.

nextest runs every `#[test]` in its own process, which is the isolation these
tests need. [`.config/nextest.toml`](../.config/nextest.toml) caps parallelism
so concurrent GPU device creation doesn't thrash the driver.

`cargo test` runs all tests of a binary in one process, and `--test-threads=1`
doesn't change that. To fail loudly instead of crashing, each
Noesis-initializing test first calls `claim_noesis_process()` (in
[`common/mod.rs`](./common/mod.rs)): the second init in a process panics with a
pointer back here.

## Layout

There are three suite binaries, each linking Bevy and Noesis once. Every file
in a suite directory is one test module, declared in that suite's `main.rs`.

- `headless_suite/`: bridge tests on `MinimalPlugins` + `NoesisHeadlessPlugin`
  (no renderer, no pipeline compilation), plus direct-Noesis unit tests.
  Driven by `common::run_until`, which steps `app.update()` until a predicate
  holds.
- `wgpu_suite/`: Noesis views rendered on `noesis_wgpu`'s device, and the
  compositing blit. No Bevy app; each test requests its own wgpu device. The
  device's own GPU tests live in `noesis_wgpu`.
- `render_suite/`: the few tests that need Bevy's real renderer from
  `DefaultPlugins`. Driven by `run_until`, then `common::settle`, which drains
  in-flight pipeline compiles before the app drops (dropping mid-compile
  segfaults at teardown).

Shared helpers live in [`common/mod.rs`](./common/mod.rs), included once per
suite `main.rs`.
