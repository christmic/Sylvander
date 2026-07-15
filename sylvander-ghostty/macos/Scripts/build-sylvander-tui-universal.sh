#!/bin/bash

set -euo pipefail

repository="${1:?usage: build-sylvander-tui-universal.sh REPOSITORY OUTPUT}"
output="${2:?usage: build-sylvander-tui-universal.sh REPOSITORY OUTPUT}"
cargo="${CARGO:-cargo}"
target_dir="${CARGO_TARGET_DIR:-${repository}/target}"
targets=(aarch64-apple-darwin x86_64-apple-darwin)

for target in "${targets[@]}"; do
    "$cargo" build \
        --manifest-path "${repository}/Cargo.toml" \
        --locked \
        --release \
        --target "$target" \
        -p sylvander-tui
done

/bin/mkdir -p "$(/usr/bin/dirname "$output")"
/usr/bin/lipo -create \
    "${target_dir}/aarch64-apple-darwin/release/sylvander-tui" \
    "${target_dir}/x86_64-apple-darwin/release/sylvander-tui" \
    -output "$output"
/bin/chmod 0755 "$output"

archs="$(/usr/bin/lipo -archs "$output")"
for required in arm64 x86_64; do
    if [[ " $archs " != *" $required "* ]]; then
        echo "error: universal sylvander-tui is missing $required (has: $archs)" >&2
        exit 1
    fi
done

echo "Built universal sylvander-tui ($archs) at $output"
