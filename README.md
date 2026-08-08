# retrovert_api
API for Retrovert

## Layout

- `include/retrovert/` — the C headers plugins and hosts build against.
- `api/` — api_gen `.def` sources the generated headers come from; see `gen-bindings.sh`.
- `rust/retrovert-host/` — safe-Rust host crate, consumed by path dependency.
- `rust/abi-parity/` — bindgen gate asserting the crate's `#[repr(C)]` mirror matches
  the headers. Kept out of `retrovert-host` so consumers never build bindgen.

```bash
cd rust && cargo test
```
