#!/bin/bash
# Wrapper script that strips --check-cfg flags before passing to real rustc
# This bridges Cargo 1.85+ (emits --check-cfg) with rustc 1.75 (doesn't support it)

SCRIPT_DIR="$(dirname "$(readlink -f "$0")")"
REAL_RUSTC="$SCRIPT_DIR/rustc.real"

# Filter out --check-cfg arguments
FILTERED_ARGS=()
SKIP_NEXT=false
for arg in "$@"; do
    if $SKIP_NEXT; then
        SKIP_NEXT=false
        continue
    fi
    if [[ "$arg" == "--check-cfg" ]]; then
        SKIP_NEXT=true
        continue
    fi
    if [[ "$arg" == -Zembed-metadata=* ]]; then
        continue
    fi
    FILTERED_ARGS+=("$arg")
done

exec "$REAL_RUSTC" "${FILTERED_ARGS[@]}"
