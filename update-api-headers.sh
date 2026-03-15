#!/bin/bash
# Update vendored API headers, cmake helpers, and CI workflow in all local playback plugin repos.
# Run from the directory containing retrovert_api and playback-* repos.

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PARENT_DIR="$(dirname "$SCRIPT_DIR")"
API_INCLUDE="$SCRIPT_DIR/include/retrovert"
API_CMAKE="$SCRIPT_DIR/cmake"
API_CI="$SCRIPT_DIR/ci"

if [ ! -d "$API_INCLUDE" ]; then
    echo "Error: $API_INCLUDE not found"
    exit 1
fi

updated=0
for dir in "$PARENT_DIR"/playback-*/; do
    [ -d "$dir" ] || continue

    if [ -d "$dir/include/retrovert" ]; then
        cp -r "$API_INCLUDE"/* "$dir/include/retrovert/"
        echo "Updated headers: $(basename "$dir")"
        updated=$((updated + 1))
    fi

    if [ -d "$dir/cmake" ]; then
        cp "$API_CMAKE"/*.cmake "$dir/cmake/"
        echo "Updated cmake:   $(basename "$dir")"
    fi

    if [ -d "$API_CI" ]; then
        mkdir -p "$dir/.github/workflows"
        cp "$API_CI"/*.yml "$dir/.github/workflows/"
        echo "Updated CI:      $(basename "$dir")"
    fi
done

if [ "$updated" -eq 0 ]; then
    echo "No playback-* repos found in $PARENT_DIR"
else
    echo "Done. Updated $updated plugin(s)."
fi
