# retrovert_api
API for Retrovert

## Layout

- `include/retrovert/` — the C headers plugins and hosts build against.
- `api/` — api_gen `.def` sources the generated headers come from; see `gen-bindings.sh`.
- `rust/retrovert-host/` — Rust host crate with an audited native-FFI boundary, published from this
  repository for Cargo Git dependencies. It is intentionally not published to crates.io.
- `rust/abi-parity/` — bindgen gate asserting the crate's `#[repr(C)]` mirror matches
  the headers. Kept out of `retrovert-host` so consumers never build bindgen.
- `ci/build.yml` — CMake workflow template copied into playback plugin repositories by
  `update-api-headers.sh`; this repository's Rust checks live in `.github/workflows/`.

Changes to decode, visualization, settings, or FFI code must follow the
[runtime contract](docs/runtime-contract.md). Its steady-state requirements are tested by
the normal Rust test job.

```bash
cd rust && cargo test
```

## Using `retrovert-host` from Cargo

Use the public repository as a Git dependency and pin an exact, reviewed commit. Replace
`FULL_COMMIT_SHA` with the complete 40-character commit ID; do not depend on the mutable default
branch.

```toml
[dependencies]
retrovert-host = { git = "https://github.com/RetrovertApp/retrovert_api", rev = "FULL_COMMIT_SHA" }
```

The crate has no path or Git dependencies of its own. Cargo discovers it under the repository's
`rust/` workspace; consumers do not need a sibling `retrovert_api` checkout or the `abi-parity`
workspace member. Release tags use the `retrovert-host-vMAJOR.MINOR.PATCH` form and must never be
moved after publication; consumers should still retain the resolved commit in `Cargo.lock`.

## Minimum supported Rust version

The Rust workspace supports Rust 1.85 and later. Test the supported feature set with the
committed dependency resolution by running:

```bash
cd rust && cargo +1.85.0 test --workspace --all-features --locked
```

Raising the minimum supported Rust version is a deliberate policy change. It must ship in a
minor release and update the package metadata, lockfile, CI toolchain, and this documentation
together.

Playback has an explicit allocation seam before real-time decoding:

```rust,ignore
let player = plugin.open("song.mod", 0)?;
let mut player = player.prepare(1_024)?;
let chunk = player.read(512)?;
```

`PreparedPlayer::read` rejects requests above the prepared frame budget and performs no
host allocation at or below it.
