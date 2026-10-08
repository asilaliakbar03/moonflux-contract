# MoonFluxx — Smart Contract

Solana on-chain program for MoonFluxx (Anchor framework, Rust).

> **Confidential.** This repository is the property of Taurus Pvt Ltd. Contract logic must not be shared outside the team.

---

## Overview

| | |
|---|---|
| Program | `moonflux_curve` |
| Framework | Anchor 0.30.1 |
| Solana SDK | 1.18.26 |
| Devnet Program ID | `DrVK92avUZvKHbyxd3StwX9c3zkZf5nDNoBrgU32e1NE` |
| Source | `programs/moonflux_curve/src/lib.rs` |

### Instructions

| Instruction | Who can call | Purpose |
|-------------|--------------|---------|
| `initialize_global` | Admin (once) | Set fee, graduation cap, fee recipient |
| `update_global` | Admin | Change fee / cap / recipient |
| `toggle_pause` | Admin | Emergency pause of all trading |
| `create_pool` | Anyone | Launch a new token with a bonding curve |
| `buy` | Anyone | Buy tokens with SOL along the curve |
| `sell` | Anyone | Sell tokens back to the curve |
| `migrate` | Admin | Move a graduated curve's SOL + tokens into the **escrow PDA** |
| `create_raydium_pool` | Admin | Release escrow → create the Raydium CPMM pool (CPI) |
| `refund_escrow` | **Anyone**, after 72h | If no pool was created in time, return SOL to the creator and burn tokens |

> ⚠️ The escrow instructions (`migrate` → escrow, `create_raydium_pool`, `refund_escrow`) are **compiled but not yet deployed** to devnet. The currently deployed version sends migrated funds to the admin wallet.

### PDA seeds

| Account | Seeds |
|---------|-------|
| Global config | `["global"]` |
| Bonding curve | `["curve", mint]` |
| Curve SOL vault | `["sol_vault", mint]` |
| Escrow | `["escrow", mint]` |
| Escrow SOL vault | `["escrow_vault", mint]` |

---

## Building

> The stock Solana toolchain (platform-tools v1.41) ships Cargo 1.75, which can't parse newer crates.io packages (`edition = "2024"`). The build therefore uses a patched toolchain. **Build inside WSL Ubuntu / Linux.**

```bash
bash install-wsl.sh     # one-time: Rust, Solana CLI 1.18.26, Anchor 0.30.1 (via AVM)
bash build-wsl.sh       # downloads platform-tools, swaps in Cargo 1.85 + rustc-wrapper.sh, builds
```

Output: `target/deploy/moonflux_curve.so`

How the workaround works:
- `rustc-wrapper.sh` replaces platform-tools `rustc` and strips `--check-cfg` / `-Zembed-metadata` flags that the old rustc doesn't understand
- `Cargo.lock` pins several crates (`blake3 1.5.1`, `borsh 1.5.7`, `indexmap 2.7.1`, `hashbrown 0.15.5`, …) to versions compatible with rustc 1.75. **Don't run `cargo update` without re-checking the build.**

---

## Deploying

**Deploys are done by the Founder only.** The deployer wallet and program keypair are never shared or committed.

- Local (devnet): `bash deploy-wsl.sh` (reads `SOLANA_RPC_URL`, `DEPLOYER_WALLET`, `PROGRAM_KEYPAIR` from env)
- GitHub Actions: **Build & Deploy Anchor Program** (manual trigger, restricted to the Founder)

After any deploy that changes instructions or accounts, copy the new IDL (`target/idl/moonflux_curve.json`) into the web repo at `lib/idl.json`.

## Testing on devnet

Use **your own** devnet wallet:
```bash
solana-keygen new
solana airdrop 2 --url devnet
```
The end-to-end test scripts live in the web repo (`scripts/e2e-step*.js`).

---

## Contributing

- Branch from `develop`, open a PR, and get **2 approvals** (Tech Lead + Founder) for any change to `programs/`
- Every new instruction or account change needs: tests, an updated instruction table above, and a note on the security impact in the PR
- Use checked math (`checked_add`, `checked_mul`, …) for all amounts. Never use unchecked arithmetic on lamports or token amounts
- Never commit keypairs. `.gitignore` covers `wallet.json` and `*-keypair.json`, but check your diff anyway
