#!/bin/bash
set -e
source ~/.cargo/env

echo "=== Installing Solana CLI ==="
sh -c "$(curl -sSfL https://release.anza.xyz/v1.18.26/install)"
export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"
solana --version

echo "=== Installing AVM (Anchor Version Manager) ==="
cargo install --git https://github.com/coral-xyz/anchor avm --force

echo "=== Installing Anchor 0.30.1 ==="
avm install 0.30.1
avm use 0.30.1
anchor --version

echo "=== DONE ==="
