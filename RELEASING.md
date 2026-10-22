# Releasing

Two crates ship from this repo in lockstep: `noesis_bevy_derive` (in
`derive/`) and `noesis_bevy`, which depends on it. Both depend on the Noesis SDK
through `noesis_runtime`.

## CI

Only a self-hosted runner with the SDK can build the crate.

- `fmt` and `doc` run on hosted runners for every push and PR, including
  forks. `doc` sets `DOCS_RS=1`, which makes the build scripts skip the native
  build.

The SDK jobs run in the private
[`dead-money/noesis-ci`](https://github.com/dead-money/noesis-ci) repo on the
self-hosted runner (label `noesis-sdk`), which takes no jobs from public repos.
Every 15 minutes it polls this repo and runs clippy and the
tests (under `cargo nextest`, see `tests/README.md`) on each new `main` commit,
and the release for each new `v*` tag. Results show in that repo's Actions tab,
not as checks here, so PRs merge on the hosted checks alone.

## `noesis_runtime` and `noesis_wgpu` versions

`noesis_bevy` depends on the published `noesis_runtime` and `noesis_wgpu`. To
develop against an unreleased version, add a local `[patch.crates-io]` pointing
at the sibling checkout and don't commit it. Publish those crates before
releasing anything that needs them.

`noesis_wgpu` keeps a release line per `wgpu` major. Use the line on the `wgpu`
Bevy uses; its `release/0.N` branch takes the fixes this crate needs.

## Cutting a release

`main` requires a pull request and passing checks, so prepare the release on
a branch. Bump both package versions (`Cargo.toml` and `derive/Cargo.toml`)
and the `noesis_bevy_derive` requirement if the minor changes. Refresh
`Cargo.lock`, stamp the changelog with the version and date, update its compare
links, and update the README's quick start and compatibility table.

Open a pull request, wait for CI to pass, and merge it. From a clean, current
`main`, tag the merged release commit (replace the version below as needed):

```sh
git pull --ff-only
git tag v0.16.0
git push origin v0.16.0
```

Within 15 minutes `dead-money/noesis-ci` notices
the tag and runs its `release.yml` on the SDK runner, which tests, then publishes `noesis_bevy_derive` and `noesis_bevy` in
that order through crates.io Trusted Publishing. Afterward, check both crate
pages and the docs.rs builds.

Keep `## [Unreleased]` in `CHANGELOG.md` current as PRs land.
