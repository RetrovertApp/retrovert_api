#!/usr/bin/env bash
# Regenerate the four headers that api/*.def owns - audio_format.h, output.h,
# playback.h, resample.h - via flowi's api_gen. The other six headers in
# include/retrovert/ are hand-written and untouched, as is the repr(C) mirror in
# rust/retrovert-host, which rust/abi-parity gates against these headers. Fan the
# result out with playback_plugins' scripts/update-api-headers.sh afterwards.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
parent="$(dirname "$here")"
api_gen="${FLOWI_API_GEN:-${parent}/../flowi/rust/tools/api_gen/Cargo.toml}"

exec cargo run --quiet --manifest-path "$api_gen" -- \
    --naming RV,rv \
    --api-dir "${here}/api" \
    --c-include-root "${here}/include" \
    --c-extern-c
