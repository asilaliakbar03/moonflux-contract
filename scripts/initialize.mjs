/**
 * One-time initializer for MoonFluxx — pure @solana/web3.js, no Anchor needed.
 * Run: node scripts/initialize.mjs
 */
import {
  Connection, Keypair, PublicKey, Transaction,
  TransactionInstruction, SystemProgram
} from '@solana/web3.js';
import { readFileSync } from 'fs';
import { createHash } from 'crypto';
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';

const __dirname = dirname(fileURLToPath(import.meta.url));

// ── Config ──────────────────────────────────────────────────────────────────
const PROGRAM_ID  = new PublicKey('DrVK92avUZvKHbyxd3StwX9c3zkZf5nDNoBrgU32e1NE');
const RPC_URL     = 'https://api.devnet.solana.com';
const WALLET_PATH = join(__dirname, '..', 'wallet.json');

// Devnet USDC/USDT (placeholders for Phase 2)
const DEVNET_USDC = new PublicKey('4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU');
const DEVNET_USDT = new PublicKey('Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB');

// ── Load deployer wallet ─────────────────────────────────────────────────────
const raw      = JSON.parse(readFileSync(WALLET_PATH, 'utf8'));
const deployer = Keypair.fromSecretKey(Uint8Array.from(raw));
console.log('Deployer:', deployer.publicKey.toBase58());

// ── Connection ───────────────────────────────────────────────────────────────
const connection = new Connection(RPC_URL, 'confirmed');

// ── Derive globalConfig PDA ──────────────────────────────────────────────────
const [globalConfig, bump] = PublicKey.findProgramAddressSync(
  [Buffer.from('global')],
  PROGRAM_ID
);
console.log('GlobalConfig PDA:', globalConfig.toBase58());

// ── Check if already initialized ────────────────────────────────────────────
const existing = await connection.getAccountInfo(globalConfig);
if (existing) {
  console.log('✅ Already initialized! GlobalConfig account exists.');
  process.exit(0);
}

// ── Build discriminator: sha256("global:initialize_global")[0..8] ────────────
// Anchor discriminator = first 8 bytes of SHA256("global:<instruction_name>")
const discriminator = createHash('sha256')
  .update('global:initialize_global')
  .digest()
  .slice(0, 8);

// ── Encode parameters (Borsh/little-endian) ──────────────────────────────────
// fee_bps: u16 LE
const feeBps = 25;
const feeBuf = Buffer.alloc(2);
feeBuf.writeUInt16LE(feeBps, 0);

// target_cap: u64 LE (85 SOL = 85_000_000_000 lamports)
const targetCap = BigInt(85_000_000_000);
const capBuf = Buffer.alloc(8);
capBuf.writeBigUInt64LE(targetCap, 0);

// usdc_mint: 32 bytes
const usdcBuf = DEVNET_USDC.toBuffer();

// usdt_mint: 32 bytes
const usdtBuf = DEVNET_USDT.toBuffer();

const data = Buffer.concat([discriminator, feeBuf, capBuf, usdcBuf, usdtBuf]);

// ── Build instruction ────────────────────────────────────────────────────────
const ix = new TransactionInstruction({
  programId: PROGRAM_ID,
  keys: [
    { pubkey: globalConfig,          isSigner: false, isWritable: true  }, // globalConfig
    { pubkey: deployer.publicKey,    isSigner: true,  isWritable: true  }, // admin
    { pubkey: deployer.publicKey,    isSigner: false, isWritable: false }, // feeRecipient
    { pubkey: SystemProgram.programId, isSigner: false, isWritable: false }, // systemProgram
  ],
  data,
});

// ── Send transaction ─────────────────────────────────────────────────────────
const { blockhash, lastValidBlockHeight } = await connection.getLatestBlockhash();
const tx = new Transaction({ recentBlockhash: blockhash, feePayer: deployer.publicKey });
tx.add(ix);
tx.sign(deployer);

console.log('Sending initialize_global transaction...');
const sig = await connection.sendRawTransaction(tx.serialize(), { skipPreflight: false });
await connection.confirmTransaction({ signature: sig, blockhash, lastValidBlockHeight }, 'confirmed');

console.log('');
console.log('✅ SUCCESS! MoonFluxx program initialized.');
console.log('Tx:', sig);
console.log('Explorer: https://solscan.io/tx/' + sig + '?cluster=devnet');
console.log('');
console.log('🚀 Users can now launch tokens at https://moonflux-mvp.vercel.app/launch');
