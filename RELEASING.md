# Releasing

Two crates ship from this repo in lockstep: `noesis_bevy_derive` (in
`derive/`) and `noesis_bevy`, which depends on it. The plugin links the Noesis SDK
through `noesis_runtime`; the derive crate needs no SDK.

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
and validates each new `v*` tag. Results show in that repo's Actions tab,
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

The private `dead-money/noesis-ci` poller validates the tag on the SDK runner.
Run its `poll.yml` manually to start immediately. After that run succeeds,
confirm the exact tag SHA and dispatch this repository's `release.yml`:

```sh
tag=vX.Y.Z
sha=$(git rev-parse "$tag^{commit}")
gh workflow run release.yml --repo dead-money/noesis_bevy --ref main \
  -f tag="$tag" -f sha="$sha" \
  -f sdk_run="https://github.com/dead-money/noesis-ci/actions/runs/<successful-run-id>"
```

The release operator must check the private run succeeded for this exact source;
the public workflow records that URL and checks tag identity, but cannot read
private CI results itself. Publishing runs on a hosted runner with `--no-verify`
because the native build and tests already passed on the SDK runner. The trusted
publisher is `dead-money/noesis_bevy`, workflow `release.yml`, no environment.
The workflow publishes `noesis_bevy_derive` before `noesis_bevy`.
Afterward, check the crate pages and docs.rs builds.

Keep `## [Unreleased]` in `CHANGELOG.md` current as PRs land.
