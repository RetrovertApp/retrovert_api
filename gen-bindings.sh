#!/usr/bin/env bash
# Regenerate the plugin ABI bindings from api/*.def via flowi's api_gen:
# C headers in include/retrovert/ and Rust FFI in retrovert-core/plugin_types/src/generated/.
# Run update-api-headers.sh afterwards to fan the headers out to the plugin repos.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repos="$(dirname "$here")"
api_gen="${FLOWI_API_GEN:-${repos}/../../flowi/rust/tools/api_gen/Cargo.toml}"

exec cargo run --quiet --manifest-path "$api_gen" -- \
    --naming RV,rv \
    --api-dir "${here}/api" \
    --rust-dir "${repos}/retrovert-core/plugin_types/src/generated" \
    --c-include-root "${here}/include" \
    --c-extern-c
