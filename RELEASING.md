# Releasing

How `noesis_bevy` gets published to crates.io.

## The constraints that shape everything

1. The crate links the closed-source Noesis SDK at build time (transitively, via
   `noesis_runtime`), so it can only be built where the SDK is present. We run a
   **self-hosted GitHub Actions runner** (label `noesis-sdk`, the same machine
   the `noesis_runtime` repo uses) that has the SDK installed and `NOESIS_SDK_DIR`
   set. That runner does the real build, clippy, test, and verified publish.
   GitHub-hosted runners only run `fmt` and `doc` (no SDK needed) and never run
   fork-PR code against the SDK box.
2. **Two crates ship from this repo:** `noesis_bevy_derive` (the
   `#[derive(NoesisViewModel)]` macro) and `noesis_bevy`. The derive crate must be
   published first, because `noesis_bevy` depends on it. They version in lockstep.
3. **`noesis_bevy` depends on `noesis_runtime`**, and this release needs runtime
   `0.10` (it uses APIs not in `0.9`). Runtime `0.10` must be on crates.io before
   `noesis_bevy` can publish. Until it is, `Cargo.toml` patches `noesis_runtime`
   to the sibling checkout (`[patch.crates-io]`), and CI checks out
   `noesis_runtime` alongside this repo.
4. The initial release is **Linux only** (`build.rs` is Linux-only; Windows
   linking is not done yet).

## CI

- **`fmt`** and **`doc`** (hosted) run on every push and PR, including forks. The
  `doc` job sets `DOCS_RS=1` so both build scripts skip the native compile.
- **`build • clippy • test`** (self-hosted) runs on pushes to `main`, version
  tags, and same-repo PRs. Fork PRs are skipped so untrusted code never touches
  the SDK runner.

The `doc` and `build-test` jobs check out `noesis_runtime` as a sibling because of
the `[patch]` above. Both repos must be public for the token-free checkout.

## Going public / first publish

The first publish is deliberate and manual (crates.io does not allow Trusted
Publishing for a crate that does not exist yet). In order:

1. **Publish `noesis_runtime` 0.10** from its own repo (see that repo's
   RELEASING.md). Nothing here can publish until it is on crates.io.
2. **Drop the sibling patch.** Remove the `[patch.crates-io]` block from
   `Cargo.toml`; with runtime 0.10 on crates.io it resolves normally. You can also
   remove the sibling-checkout steps from the CI workflows.
3. **Make both crates publishable.** Remove `publish = false` from `Cargo.toml`
   and `derive/Cargo.toml`.
4. **Claim the names with a manual first publish**, derive first. Create a
   crates.io API token (scope `publish-new`), then from a machine with the SDK:

   ```sh
   cd derive && CARGO_REGISTRY_TOKEN=<token> cargo publish && cd ..
   CARGO_REGISTRY_TOKEN=<token> NOESIS_SDK_DIR=~/sdk/noesis-3.2.13 cargo publish
   ```

   Revoke the token afterward; later releases use Trusted Publishing.
5. **Configure crates.io Trusted Publishing** for both crates
   (`noesis_bevy_derive` and `noesis_bevy`). On crates.io, the crate, Settings,
   Trusted Publishing, add a GitHub publisher:
   - Repository: `dead-money/noesis_bevy`
   - Workflow filename: `release.yml`
   - Environment: leave blank.

   It uses GitHub's OIDC identity, so there is no API token to store.

## Cutting a subsequent release

With `main` clean and CI green, and `cargo-release` installed:

```sh
cargo release 0.11.0        # or: patch | minor | major
```

`cargo release` (config in `release.toml`) bumps `Cargo.toml`, stamps
`CHANGELOG.md`, commits, tags `vX.Y.Z`, and pushes. **Bump `derive/Cargo.toml` to
the same version in that commit by hand** (it is not a workspace member, so
cargo-release does not touch it). The pushed tag triggers `release.yml` on the
self-hosted runner, which tests, then publishes the derive crate and `noesis_bevy`
in order via Trusted Publishing.

Do a dry run first:

```sh
cargo release 0.11.0 --dry-run
```

After it lands, confirm both crates on crates.io and that docs.rs built (it builds
without the SDK because the build scripts short-circuit on `DOCS_RS`).

## The self-hosted runner

The same Ubuntu droplet that serves `noesis_runtime` (label `noesis-sdk`). It must
be registered to this repo as well (or as an org runner). SDK at its configured
path with `NOESIS_SDK_DIR` set; runs as the unprivileged `runner` user; `target/`
kept between runs (`clean: false`) for fast incremental builds.
