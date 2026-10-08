#!/bin/bash
set -e
source ~/.cargo/env
export PATH="$HOME/.local/share/solana/install/active_release/bin:$HOME/.avm/bin:$PATH"

PLATFORM="$HOME/.cache/solana/v1.41/platform-tools"
CARGO_185="$HOME/.rustup/toolchains/1.85.0-x86_64-unknown-linux-gnu/bin/cargo"

echo "=== Fresh platform-tools state ==="
echo "rustc (17KB static): $($PLATFORM/rust/bin/rustc --version)"
echo "cargo (original): $($PLATFORM/rust/bin/cargo --version)"

echo ""
echo "=== Patch: rustc.real + wrapper + cargo 1.85 ==="
cp "$PLATFORM/rust/bin/rustc" "$PLATFORM/rust/bin/rustc.real"
cp /mnt/c/mvp/moonflux-contract/rustc-wrapper.sh "$PLATFORM/rust/bin/rustc"
chmod +x "$PLATFORM/rust/bin/rustc"
cp "$CARGO_185" "$PLATFORM/rust/bin/cargo"

echo "cargo: $($PLATFORM/rust/bin/cargo --version)"
echo "rustc.real: $($PLATFORM/rust/bin/rustc.real --version)"

echo ""
echo "=== Clear ==="
rm -rf ~/.cargo/registry/src/index.crates.io-*
rm -rf ~/.cargo/registry/cache/index.crates.io-*
rustup toolchain uninstall solana 2>/dev/null || true
cd /mnt/c/mvp/moonflux-contract
rm -rf target/release target/sbf-solana-solana target/deploy 2>/dev/null || true

echo ""
echo "=== Build ==="
cargo-build-sbf --manifest-path programs/moonflux_curve/Cargo.toml 2>&1 | tail -100

echo ""
echo "=== Result ==="
ls -la target/deploy/ 2>/dev/null || echo "No deploy directory"
