# Releasing

Two crates ship from this repo in lockstep: `noesis_bevy_derive` (in
`derive/`) and `noesis_bevy`, which depends on it. Both depend on the Noesis SDK
through `noesis_runtime`.

## CI

Only a self-hosted runner with the SDK (label `noesis-sdk`) can build the crate.

- `fmt` and `doc` run on hosted runners for every push and PR, including
  forks. `doc` sets `DOCS_RS=1`, which makes the build scripts skip the native
  build.
- `build • clippy • test` runs on the SDK runner for pushes to `main` and
  same-repo PRs. Fork PRs are skipped. Tests run under `cargo nextest` (see
  `tests/README.md`).

## `noesis_runtime` versions

`noesis_bevy` depends on the published `noesis_runtime`. To develop against an
unreleased runtime, add a local `[patch.crates-io]` pointing at the sibling
checkout and don't commit it. Publish the runtime before releasing anything
that needs it.

## Cutting a release

Requires [cargo-release](https://github.com/crate-ci/cargo-release), which
bumps only `noesis_bevy`. First, in a commit on `main`, bump `derive/Cargo.toml`
to the new version, and the `noesis_bevy_derive` requirement in `Cargo.toml` if
the minor changes. Then, with `main` clean and CI green (use `patch` for a
patch release):

```sh
cargo release minor --dry-run
cargo release minor --execute
```

It bumps the version, stamps `CHANGELOG.md`, commits, tags `vX.Y.Z`, and
pushes. It doesn't update the link references at the bottom of
`CHANGELOG.md`: move the `[Unreleased]` link to the new tag and add the new
version's compare link by hand. The tag triggers `release.yml` on the SDK
runner, which tests, then publishes `noesis_bevy_derive` and `noesis_bevy` in
that order through crates.io Trusted Publishing. Afterward, check both crate
pages and the docs.rs builds.

Keep `## [Unreleased]` in `CHANGELOG.md` current as PRs land.
