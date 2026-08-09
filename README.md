# retrovert_api
API for Retrovert

## Layout

- `include/retrovert/` — the C headers plugins and hosts build against.
- `api/` — api_gen `.def` sources the generated headers come from; see `gen-bindings.sh`.
- `rust/retrovert-host/` — safe-Rust host crate, consumed by path dependency.
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

Playback has an explicit allocation seam before real-time decoding:

```rust,ignore
let player = plugin.open("song.mod", 0)?;
let mut player = player.prepare(1_024)?;
let chunk = player.read(512)?;
```

`PreparedPlayer::read` rejects requests above the prepared frame budget and performs no
host allocation at or below it.
