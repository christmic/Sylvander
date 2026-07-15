#!/bin/bash

set -euo pipefail

if [[ "${PLATFORM_NAME:-macosx}" != "macosx" ]]; then
    exit 0
fi

profile="debug"
if [[ "${CONFIGURATION:-Debug}" != "Debug" ]]; then
    profile="release"
fi

source_binary="${SYLVANDER_TUI_PATH:-${SRCROOT}/../../target/${profile}/sylvander-tui}"
destination_dir="${TARGET_BUILD_DIR}/${UNLOCALIZED_RESOURCES_FOLDER_PATH}/bin"
destination_binary="${destination_dir}/sylvander-tui"

if [[ ! -x "$source_binary" ]]; then
    echo "error: sylvander-tui helper is missing or not executable: $source_binary" >&2
    echo "error: build it with 'cargo build -p sylvander-tui' or set SYLVANDER_TUI_PATH" >&2
    exit 1
fi

available_archs="$(/usr/bin/lipo -archs "$source_binary")"
for required_arch in ${ARCHS:-}; do
    if [[ " $available_archs " != *" $required_arch "* ]]; then
        echo "error: sylvander-tui is missing required architecture '$required_arch' (has: $available_archs)" >&2
        exit 1
    fi
done

/bin/mkdir -p "$destination_dir"
/usr/bin/install -m 0755 "$source_binary" "$destination_binary"

if [[ "${CODE_SIGNING_ALLOWED:-NO}" == "YES" && -n "${EXPANDED_CODE_SIGN_IDENTITY:-}" ]]; then
    signing_options=(--force --sign "$EXPANDED_CODE_SIGN_IDENTITY")
    if [[ "${CONFIGURATION:-Debug}" != "Debug" ]]; then
        signing_options+=(--options runtime)
        if [[ "$EXPANDED_CODE_SIGN_IDENTITY" == "-" ]]; then
            signing_options+=(--timestamp=none)
        else
            signing_options+=(--timestamp)
        fi
    fi
    /usr/bin/codesign "${signing_options[@]}" "$destination_binary"
fi

echo "Embedded sylvander-tui ($available_archs) at $destination_binary"
