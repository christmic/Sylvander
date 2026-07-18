#!/bin/bash

set -euo pipefail

repository="${1:?usage: build-sylvander-tui-universal.sh REPOSITORY OUTPUT}"
output="${2:?usage: build-sylvander-tui-universal.sh REPOSITORY OUTPUT}"
target_dir="${CARGO_TARGET_DIR:-${repository}/target}"
targets=(aarch64-apple-darwin x86_64-apple-darwin)

if [[ -n "${CARGO:-}" ]]; then
    cargo_command=("$CARGO")
elif command -v rustup >/dev/null 2>&1 && rustup which cargo >/dev/null 2>&1; then
    toolchain="$(rustup show active-toolchain | awk '{print $1}')"
    cargo_command=("$(rustup which cargo --toolchain "$toolchain")")
    rustc_command="$(rustup which rustc --toolchain "$toolchain")"
    for target in "${targets[@]}"; do
        if ! rustup target list --installed --toolchain "$toolchain" | grep -qx "$target"; then
            echo "error: Rust target '$target' is not installed for '$toolchain'" >&2
            echo "install it with: rustup target add '$target' --toolchain '$toolchain'" >&2
            exit 1
        fi
    done
else
    cargo_command=(cargo)
fi

for target in "${targets[@]}"; do
    if [[ -n "${rustc_command:-}" ]]; then
        RUSTC="$rustc_command" "${cargo_command[@]}" build \
            --manifest-path "${repository}/Cargo.toml" \
            --locked \
            --release \
            --target "$target" \
            -p sylvander-tui
    else
        "${cargo_command[@]}" build \
            --manifest-path "${repository}/Cargo.toml" \
            --locked \
            --release \
            --target "$target" \
            -p sylvander-tui
    fi
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
