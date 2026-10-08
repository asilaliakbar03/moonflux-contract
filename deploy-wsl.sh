#!/bin/bash
# Deploy the compiled program to devnet.
#
# Required env (or defaults):
#   SOLANA_RPC_URL   RPC endpoint (default: public devnet)
#   DEPLOYER_WALLET  path to deployer keypair (default: wallet.json)
#   PROGRAM_KEYPAIR  path to program keypair  (default: program-keypair.json)
#
# Keypair files are gitignored and must NEVER be committed.
set -e
source ~/.cargo/env
export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"

cd "$(dirname "$0")"

RPC_URL="${SOLANA_RPC_URL:-https://api.devnet.solana.com}"
DEPLOYER_WALLET="${DEPLOYER_WALLET:-wallet.json}"
PROGRAM_KEYPAIR="${PROGRAM_KEYPAIR:-program-keypair.json}"

echo "=== Solana CLI ==="
solana --version

echo "=== Program binary ==="
ls -la target/deploy/moonflux_curve.so

echo "=== Deploying to devnet ==="
solana program deploy \
    target/deploy/moonflux_curve.so \
    --program-id "$PROGRAM_KEYPAIR" \
    --keypair "$DEPLOYER_WALLET" \
    --url "$RPC_URL" \
    -v 2>&1

echo ""
echo "=== DONE ==="
