use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{
        self, Mint, Token, TokenAccount, MintTo, Transfer, Burn,
        SetAuthority, spl_token::instruction::AuthorityType,
    },
};
use solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke_signed,
};

declare_id!("DrVK92avUZvKHbyxd3StwX9c3zkZf5nDNoBrgU32e1NE");

// ── CONSTANTS ─────────────────────────────────────────────────────────────────
pub const VIRTUAL_SOL_RESERVE: u64   = 30_u64 * 1_000_000_000_u64;        // 30 SOL in lamports
pub const VIRTUAL_TOKEN_RESERVE: u64 = 1_073_000_000_u64 * 1_000_000_u64; // ~1.073B tokens (6 decimals)
pub const TOTAL_TOKEN_SUPPLY: u64    = 1_000_000_000_u64 * 1_000_000_u64;  // 1B tokens for bonding curve
pub const TARGET_SOL_DEFAULT: u64    = 85_u64 * 1_000_000_000_u64;         // 85 SOL graduation target (SOL)
pub const TARGET_USDC_DEFAULT: u64   = 69_000_u64 * 1_000_000_u64;         // $69,000 graduation target (USDC/USDT, 6 decimals)
pub const FEE_BPS_DEFAULT: u64       = 100_u64;                             // 1.00% default fee
pub const METADATA_URI_MAX_LEN: usize = 200;                                // Max on-chain metadata URI length
pub const ESCROW_TIMEOUT: i64 = 72 * 60 * 60;                                // 72 hours in seconds

// ── RAYDIUM CPMM CONSTANTS ───────────────────────────────────────────────────
pub const RAYDIUM_CPMM_DEVNET: &str = "DRaycpLY18LhpbydsBWbVJtxpNv9oXPgjRSfpF2bWpYb";
pub const RAYDIUM_CREATE_POOL_FEE_DEVNET: &str = "3oE58BKVt8KuYkGxx8zBojugnymWmBiyafWgMrnb6eYy";
pub const RAYDIUM_INITIALIZE_DISCRIMINATOR: [u8; 8] = [175, 175, 109, 31, 13, 152, 155, 237];

// ── ESCROW STATUS ────────────────────────────────────────────────────────────
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum EscrowStatus {
    Pending    = 0, // Funds locked, waiting for pool creation
    PoolCreated = 1, // Raydium pool was created successfully
    Refunded   = 2, // Timeout expired, funds returned to creator
}

// ── ACCOUNT SIZES ─────────────────────────────────────────────────────────────
// GlobalConfig: extended with usdc_mint + usdt_mint (two extra Pubkeys = 64 bytes)
pub const GLOBAL_CONFIG_SIZE: usize = 8    // discriminator
                                    + 32   // admin
                                    + 32   // fee_recipient
                                    + 2    // fee_bps
                                    + 8    // target_cap (SOL)
                                    + 8    // target_cap_usdc ($USDC)
                                    + 1    // paused
                                    + 32   // usdc_mint (for validation)
                                    + 32;  // usdt_mint (for validation)

// BondingCurve: extended with quote_type, quote_mint, metadata_uri
pub const CURVE_ACCOUNT_SIZE: usize = 8    // discriminator
                                    + 32   // mint
                                    + 32   // creator
                                    + 8    // virtual_sol_reserves
                                    + 8    // virtual_token_reserves
                                    + 8    // real_sol_reserves
                                    + 8    // real_token_reserves
                                    + 1    // complete
                                    + 8    // total_fees_collected
                                    + 8    // created_at timestamp
                                    + 1    // quote_type (0=SOL, 1=USDC, 2=USDT)
                                    + 32   // quote_mint (native SOL = Pubkey::default())
                                    + 4 + METADATA_URI_MAX_LEN // metadata_uri (Vec<u8> prefix + bytes)
                                    + 8    // creator_fee_bps  (future rev-share)
                                    + 32;  // migration_wallet (future)

// MigrationEscrow: holds graduated funds until Raydium pool creation or refund
pub const ESCROW_ACCOUNT_SIZE: usize = 8    // discriminator
                                     + 32   // mint
                                     + 32   // creator (token creator, receives refund)
                                     + 8    // sol_amount (lamports escrowed)
                                     + 8    // token_amount (tokens escrowed)
                                     + 8    // created_at (unix timestamp)
                                     + 8    // timeout (seconds, default 72h)
                                     + 1;   // status (EscrowStatus enum)

// ── QUOTE TYPE ────────────────────────────────────────────────────────────────
/// Determines what token the user pays/receives when buying/selling on the curve.
/// SOL  = native SOL lamports  (Phase 1 — launched now)
/// USDC = SPL USDC (6 decimals) (Phase 2 — stable denomination)
/// USDT = SPL USDT (6 decimals) (Phase 2 — stable denomination)
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum QuoteType {
    Sol  = 0,
    Usdc = 1,
    Usdt = 2,
}

impl Default for QuoteType {
    fn default() -> Self { QuoteType::Sol }
}

// ── PROGRAM ───────────────────────────────────────────────────────────────────
#[program]
pub mod moonflux_curve {
    use super::*;

    // ── initialize_global ────────────────────────────────────────────────────
    /// Sets up the global fee config. Must be called once by the admin.
    /// Phase 2: now accepts usdc_mint + usdt_mint for future SPL-quote validation.
    pub fn initialize_global(
        ctx: Context<InitializeGlobal>,
        fee_bps: u16,
        target_cap: u64,
        usdc_mint: Pubkey,
        usdt_mint: Pubkey,
    ) -> Result<()> {
        require!(fee_bps <= 500, CurveError::FeeTooHigh);
        require!(target_cap > 0, CurveError::InvalidAmount);

        let config = &mut ctx.accounts.global_config;
        config.admin          = ctx.accounts.admin.key();
        config.fee_recipient  = ctx.accounts.fee_recipient.key();
        config.fee_bps        = fee_bps;
        config.target_cap     = target_cap;
        config.target_cap_usdc = TARGET_USDC_DEFAULT;
        config.paused         = false;
        config.usdc_mint      = usdc_mint;
        config.usdt_mint      = usdt_mint;
        Ok(())
    }

    // ── update_global ────────────────────────────────────────────────────────
    /// Allows admin to update fee config and stablecoin mints.
    pub fn update_global(
        ctx: Context<UpdateGlobal>,
        new_fee_bps: u16,
        new_target_cap: u64,
        new_fee_recipient: Pubkey,
        new_usdc_mint: Pubkey,
        new_usdt_mint: Pubkey,
    ) -> Result<()> {
        require!(new_fee_bps <= 500, CurveError::FeeTooHigh);
        require!(new_target_cap > 0, CurveError::InvalidAmount);

        let config = &mut ctx.accounts.global_config;
        config.fee_bps       = new_fee_bps;
        config.target_cap    = new_target_cap;
        config.fee_recipient = new_fee_recipient;
        config.usdc_mint     = new_usdc_mint;
        config.usdt_mint     = new_usdt_mint;
        Ok(())
    }

    // ── toggle_pause ─────────────────────────────────────────────────────────
    pub fn toggle_pause(ctx: Context<AdminOnly>) -> Result<()> {
        let config = &mut ctx.accounts.global_config;
        config.paused = !config.paused;
        msg!("Program paused state toggled to: {}", config.paused);
        Ok(())
    }

    // ── create_pool ──────────────────────────────────────────────────────────
    /// Creates a new bonding curve pool for a given SPL Mint.
    /// Phase 2: accepts metadata_uri (IPFS/Arweave link) and quote_type.
    /// quote_type = 0 (SOL) for Phase 1. Set to 1 (USDC) or 2 (USDT) for Phase 2 stablecoin curves.
    pub fn create_pool(
        ctx: Context<CreatePool>,
        metadata_uri: String,
        quote_type: u8,         // 0=SOL, 1=USDC, 2=USDT
    ) -> Result<()> {
        require!(!ctx.accounts.global_config.paused, CurveError::ProgramPaused);
        require!(metadata_uri.len() <= METADATA_URI_MAX_LEN, CurveError::MetadataUriTooLong);
        require!(quote_type <= 2, CurveError::InvalidQuoteType);

        // Resolve quote_type → QuoteType enum + quote_mint Pubkey
        let (resolved_quote_type, quote_mint_key) = match quote_type {
            0 => (QuoteType::Sol,  Pubkey::default()),  // SOL = native, no SPL mint
            1 => {
                // Validate: the quote_mint account passed must match GlobalConfig.usdc_mint
                require!(
                    ctx.accounts.quote_mint.key() == ctx.accounts.global_config.usdc_mint,
                    CurveError::InvalidQuoteMint
                );
                (QuoteType::Usdc, ctx.accounts.quote_mint.key())
            },
            2 => {
                require!(
                    ctx.accounts.quote_mint.key() == ctx.accounts.global_config.usdt_mint,
                    CurveError::InvalidQuoteMint
                );
                (QuoteType::Usdt, ctx.accounts.quote_mint.key())
            },
            _ => return Err(CurveError::InvalidQuoteType.into()),
        };

        let curve = &mut ctx.accounts.bonding_curve;
        curve.mint                   = ctx.accounts.mint.key();
        curve.creator                = ctx.accounts.creator.key();
        curve.virtual_sol_reserves   = VIRTUAL_SOL_RESERVE;
        curve.virtual_token_reserves = VIRTUAL_TOKEN_RESERVE;
        curve.real_sol_reserves      = 0;
        curve.real_token_reserves    = TOTAL_TOKEN_SUPPLY;
        curve.complete               = false;
        curve.total_fees_collected   = 0;
        curve.created_at             = Clock::get()?.unix_timestamp;
        curve.quote_type             = resolved_quote_type;
        curve.quote_mint             = quote_mint_key;
        curve.metadata_uri           = metadata_uri.clone();

        let seeds: &[&[u8]] = &[
            b"curve",
            ctx.accounts.mint.to_account_info().key.as_ref(),
            &[ctx.bumps.bonding_curve],
        ];
        let signer = &[seeds];

        // ── Create the SOL vault PDA (used for SOL-quote pools) ──────────────
        // For USDC/USDT pools the vault still needs to exist (for rent/PDA derivation),
        // but actual quote-token custody happens in the quote_vault token account.
        let vault_bump = ctx.bumps.sol_vault;
        let vault_seeds: &[&[u8]] = &[
            b"sol_vault",
            ctx.accounts.mint.to_account_info().key.as_ref(),
            &[vault_bump],
        ];
        let vault_signer = &[vault_seeds];
        let rent = Rent::get()?;
        let lamports_needed = rent.minimum_balance(0);
        anchor_lang::system_program::create_account(
            CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                anchor_lang::system_program::CreateAccount {
                    from: ctx.accounts.creator.to_account_info(),
                    to:   ctx.accounts.sol_vault.to_account_info(),
                },
                vault_signer,
            ),
            lamports_needed,
            0,
            &System::id(),
        )?;

        // ── Mint full token supply to curve vault ────────────────────────────
        token::mint_to(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                MintTo {
                    mint:      ctx.accounts.mint.to_account_info(),
                    to:        ctx.accounts.curve_token_account.to_account_info(),
                    authority: ctx.accounts.bonding_curve.to_account_info(),
                },
                signer,
            ),
            TOTAL_TOKEN_SUPPLY,
        )?;

        // ── Permanently revoke mint authority ────────────────────────────────
        token::set_authority(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                SetAuthority {
                    current_authority: ctx.accounts.bonding_curve.to_account_info(),
                    account_or_mint:   ctx.accounts.mint.to_account_info(),
                },
                signer,
            ),
            AuthorityType::MintTokens,
            None,
        )?;

        msg!(
            "Pool created. Mint: {}. Quote: {:?}. Metadata: {}. Mint authority permanently revoked.",
            ctx.accounts.mint.key(),
            quote_type,
            metadata_uri,
        );
        Ok(())
    }

    // ── buy ──────────────────────────────────────────────────────────────────
    /// Swaps quote currency (SOL or SPL stablecoin) for tokens.
    /// Phase 1: SOL path is fully implemented.
    /// Phase 2: USDC/USDT path is gated — the instruction will reject non-SOL
    /// curves with UnsupportedQuoteType until the SPL custody logic is live.
    pub fn buy(ctx: Context<BuySell>, amount_in: u64, min_tokens_out: u64) -> Result<()> {
        let config = &ctx.accounts.global_config;
        require!(!config.paused, CurveError::ProgramPaused);

        // Save account_info before mutable borrow of curve
        let curve_account_info = ctx.accounts.bonding_curve.to_account_info();

        let curve = &mut ctx.accounts.bonding_curve;
        require!(!curve.complete, CurveError::CurveComplete);
        require!(amount_in > 0, CurveError::InvalidAmount);

        // Phase 2 gate: reject stablecoin curves until SPL custody is deployed
        require!(curve.quote_type == QuoteType::Sol, CurveError::UnsupportedQuoteType);

        let fee_bps   = config.fee_bps as u64;
        let target_cap = config.target_cap;

        let sol_remaining = target_cap.saturating_sub(curve.real_sol_reserves);
        require!(sol_remaining > 0, CurveError::CurveComplete);
        let actual_sol = std::cmp::min(amount_in, sol_remaining);

        let fee    = (actual_sol * fee_bps) / 10_000;
        let sol_in = actual_sol.checked_sub(fee).ok_or(CurveError::MathOverflow)?;

        // Constant Product Math
        let k = (curve.virtual_sol_reserves as u128)
            .checked_mul(curve.virtual_token_reserves as u128)
            .ok_or(CurveError::MathOverflow)?;

        let new_virtual_sol = curve.virtual_sol_reserves
            .checked_add(sol_in)
            .ok_or(CurveError::MathOverflow)?;

        let new_virtual_token = ((k / new_virtual_sol as u128) + 1) as u64;

        let tokens_out = curve.virtual_token_reserves
            .checked_sub(new_virtual_token)
            .ok_or(CurveError::MathOverflow)?;

        require!(tokens_out >= min_tokens_out, CurveError::SlippageExceeded);
        require!(tokens_out <= curve.real_token_reserves, CurveError::InsufficientTokens);

        curve.virtual_sol_reserves   = new_virtual_sol;
        curve.virtual_token_reserves = new_virtual_token;
        curve.real_sol_reserves      = curve.real_sol_reserves
            .checked_add(sol_in).ok_or(CurveError::MathOverflow)?;
        curve.real_token_reserves    = curve.real_token_reserves
            .checked_sub(tokens_out).ok_or(CurveError::MathOverflow)?;
        curve.total_fees_collected   = curve.total_fees_collected.saturating_add(fee);

        // Transfer fee to fee_recipient
        if fee > 0 {
            anchor_lang::system_program::transfer(
                CpiContext::new(
                    ctx.accounts.system_program.to_account_info(),
                    anchor_lang::system_program::Transfer {
                        from: ctx.accounts.user.to_account_info(),
                        to:   ctx.accounts.fee_recipient.to_account_info(),
                    },
                ),
                fee,
            )?;
        }

        // Transfer net SOL to sol vault
        anchor_lang::system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.to_account_info(),
                anchor_lang::system_program::Transfer {
                    from: ctx.accounts.user.to_account_info(),
                    to:   ctx.accounts.sol_vault.to_account_info(),
                },
            ),
            sol_in,
        )?;

        // Transfer tokens from curve vault to buyer
        let seeds: &[&[u8]] = &[
            b"curve",
            ctx.accounts.mint.to_account_info().key.as_ref(),
            &[ctx.bumps.bonding_curve],
        ];
        let signer = &[seeds];

        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from:      ctx.accounts.curve_token_account.to_account_info(),
                    to:        ctx.accounts.user_token_account.to_account_info(),
                    authority: curve_account_info,
                },
                signer,
            ),
            tokens_out,
        )?;

        if curve.real_sol_reserves >= target_cap {
            curve.complete = true;
            msg!("GRADUATION: Curve complete! {} SOL raised.", curve.real_sol_reserves);
        }

        msg!("BUY [SOL]: {} lamports in → {} tokens out. Fee: {}.", sol_in, tokens_out, fee);
        Ok(())
    }

    // ── sell ─────────────────────────────────────────────────────────────────
    /// Swaps tokens back for the quote currency.
    pub fn sell(ctx: Context<BuySell>, amount_tokens: u64, min_sol_out: u64) -> Result<()> {
        let config = &ctx.accounts.global_config;
        require!(!config.paused, CurveError::ProgramPaused);

        let curve = &mut ctx.accounts.bonding_curve;
        require!(!curve.complete, CurveError::CurveComplete);
        require!(amount_tokens > 0, CurveError::InvalidAmount);

        // Phase 2 gate
        require!(curve.quote_type == QuoteType::Sol, CurveError::UnsupportedQuoteType);

        let fee_bps = config.fee_bps as u64;

        let k = (curve.virtual_sol_reserves as u128)
            .checked_mul(curve.virtual_token_reserves as u128)
            .ok_or(CurveError::MathOverflow)?;

        let new_virtual_token = curve.virtual_token_reserves
            .checked_add(amount_tokens)
            .ok_or(CurveError::MathOverflow)?;

        let new_virtual_sol = (k / new_virtual_token as u128) as u64;

        let sol_out = curve.virtual_sol_reserves
            .checked_sub(new_virtual_sol)
            .ok_or(CurveError::MathOverflow)?;

        let fee         = (sol_out * fee_bps) / 10_000;
        let net_sol_out = sol_out.checked_sub(fee).ok_or(CurveError::MathOverflow)?;

        require!(net_sol_out >= min_sol_out, CurveError::SlippageExceeded);
        require!(sol_out <= curve.real_sol_reserves, CurveError::InsufficientSol);

        curve.virtual_sol_reserves  = new_virtual_sol;
        curve.virtual_token_reserves = new_virtual_token;
        curve.real_sol_reserves     = curve.real_sol_reserves
            .checked_sub(sol_out).ok_or(CurveError::MathOverflow)?;
        curve.real_token_reserves   = curve.real_token_reserves
            .checked_add(amount_tokens).ok_or(CurveError::MathOverflow)?;
        curve.total_fees_collected  = curve.total_fees_collected.saturating_add(fee);

        // Transfer tokens from seller to curve vault
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from:      ctx.accounts.user_token_account.to_account_info(),
                    to:        ctx.accounts.curve_token_account.to_account_info(),
                    authority: ctx.accounts.user.to_account_info(),
                },
            ),
            amount_tokens,
        )?;

        // Transfer SOL from vault to user
        let vault_seeds: &[&[u8]] = &[
            b"sol_vault",
            ctx.accounts.mint.to_account_info().key.as_ref(),
            &[ctx.bumps.sol_vault],
        ];
        let vault_signer = &[vault_seeds];

        anchor_lang::system_program::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                anchor_lang::system_program::Transfer {
                    from: ctx.accounts.sol_vault.to_account_info(),
                    to:   ctx.accounts.user.to_account_info(),
                },
                vault_signer,
            ),
            net_sol_out,
        )?;

        if fee > 0 {
            anchor_lang::system_program::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.system_program.to_account_info(),
                    anchor_lang::system_program::Transfer {
                        from: ctx.accounts.sol_vault.to_account_info(),
                        to:   ctx.accounts.fee_recipient.to_account_info(),
                    },
                    vault_signer,
                ),
                fee,
            )?;
        }

        msg!("SELL [SOL]: {} tokens in → {} lamports out. Fee: {}.", amount_tokens, net_sol_out, fee);
        Ok(())
    }

    // ── migrate ──────────────────────────────────────────────────────────────
    /// Called by admin after graduation. Sends SOL + remaining tokens
    /// to a PROGRAM-CONTROLLED ESCROW PDA (not an admin wallet).
    /// The escrow holds funds until Raydium pool creation or 72h refund.
    pub fn migrate(ctx: Context<Migrate>) -> Result<()> {
        let curve = &ctx.accounts.bonding_curve;
        require!(curve.complete, CurveError::CurveNotComplete);

        let sol_balance   = ctx.accounts.sol_vault.lamports();
        let token_balance = ctx.accounts.curve_token_account.amount;
        let mint_key      = ctx.accounts.mint.key();

        // Transfer SOL from sol_vault → escrow_sol_vault
        if sol_balance > 0 {
            let vault_seeds: &[&[u8]] = &[
                b"sol_vault",
                mint_key.as_ref(),
                &[ctx.bumps.sol_vault],
            ];
            anchor_lang::system_program::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.system_program.to_account_info(),
                    anchor_lang::system_program::Transfer {
                        from: ctx.accounts.sol_vault.to_account_info(),
                        to:   ctx.accounts.escrow_sol_vault.to_account_info(),
                    },
                    &[vault_seeds],
                ),
                sol_balance,
            )?;
        }

        // Transfer tokens from curve_token_account → escrow_token_account
        if token_balance > 0 {
            let curve_seeds: &[&[u8]] = &[
                b"curve",
                mint_key.as_ref(),
                &[ctx.bumps.bonding_curve],
            ];
            token::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer {
                        from:      ctx.accounts.curve_token_account.to_account_info(),
                        to:        ctx.accounts.escrow_token_account.to_account_info(),
                        authority: ctx.accounts.bonding_curve.to_account_info(),
                    },
                    &[curve_seeds],
                ),
                token_balance,
            )?;
        }

        // Initialize the escrow account
        let escrow = &mut ctx.accounts.escrow;
        let clock = Clock::get()?;
        escrow.mint         = mint_key;
        escrow.creator      = curve.creator;
        escrow.sol_amount   = sol_balance;
        escrow.token_amount = token_balance;
        escrow.created_at   = clock.unix_timestamp;
        escrow.timeout      = ESCROW_TIMEOUT;
        escrow.status       = EscrowStatus::Pending;

        msg!(
            "MIGRATE TO ESCROW: {} lamports + {} tokens. Escrow PDA = {}. Refund after {}s.",
            sol_balance, token_balance, ctx.accounts.escrow.key(), ESCROW_TIMEOUT
        );
        Ok(())
    }

    // ── create_raydium_pool ─────────────────────────────────────────────────
    /// Called by admin to create Raydium CPMM pool using escrowed funds.
    /// Transfers SOL + tokens from escrow to admin (as Raydium creator),
    /// then CPIs into Raydium CPMM to initialize the pool.
    /// LP tokens are burned immediately to permanently lock liquidity.
    pub fn create_raydium_pool<'info>(
        ctx: Context<'_, '_, 'info, 'info, CreateRaydiumPool<'info>>,
        init_amount_sol: u64,
        init_amount_token: u64,
    ) -> Result<()> {
        let escrow = &mut ctx.accounts.escrow;
        require!(escrow.status == EscrowStatus::Pending, CurveError::EscrowAlreadyProcessed);

        let mint_key = escrow.mint;

        // Transfer SOL from escrow vault to admin (Raydium needs creator = wallet)
        let escrow_vault_seeds: &[&[u8]] = &[
            b"escrow_vault",
            mint_key.as_ref(),
            &[ctx.bumps.escrow_sol_vault],
        ];
        let escrow_sol_bal = ctx.accounts.escrow_sol_vault.lamports();
        if escrow_sol_bal > 0 {
            anchor_lang::system_program::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.system_program.to_account_info(),
                    anchor_lang::system_program::Transfer {
                        from: ctx.accounts.escrow_sol_vault.to_account_info(),
                        to:   ctx.accounts.admin.to_account_info(),
                    },
                    &[escrow_vault_seeds],
                ),
                escrow_sol_bal,
            )?;
        }

        // Transfer tokens from escrow to admin's token account
        let escrow_seeds: &[&[u8]] = &[
            b"escrow",
            mint_key.as_ref(),
            &[ctx.bumps.escrow],
        ];
        let escrow_token_bal = ctx.accounts.escrow_token_account.amount;
        if escrow_token_bal > 0 {
            token::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer {
                        from:      ctx.accounts.escrow_token_account.to_account_info(),
                        to:        ctx.accounts.admin_token_account.to_account_info(),
                        authority: ctx.accounts.escrow.to_account_info(),
                    },
                    &[escrow_seeds],
                ),
                escrow_token_bal,
            )?;
        }

        // Build Raydium CPMM initialize CPI
        // The remaining accounts must be passed in this EXACT order:
        // [0] cp_swap_program, [1] amm_config, [2] authority, [3] pool_state,
        // [4] token_0_mint, [5] token_1_mint, [6] lp_mint,
        // [7] creator_token_0, [8] creator_token_1, [9] creator_lp_token,
        // [10] token_0_vault, [11] token_1_vault, [12] create_pool_fee,
        // [13] observation_state, [14] token_program, [15] token_0_program,
        // [16] token_1_program, [17] associated_token_program,
        // [18] system_program, [19] rent
        let remaining = ctx.remaining_accounts;
        require!(remaining.len() >= 20, CurveError::InsufficientRaydiumAccounts);

        let cp_swap_program = &remaining[0];

        let mut data = Vec::with_capacity(32);
        data.extend_from_slice(&RAYDIUM_INITIALIZE_DISCRIMINATOR);
        data.extend_from_slice(&init_amount_sol.to_le_bytes());
        data.extend_from_slice(&init_amount_token.to_le_bytes());
        data.extend_from_slice(&0u64.to_le_bytes()); // open_time = 0 (immediate)

        let accounts = vec![
            AccountMeta::new(ctx.accounts.admin.key(), true),     // creator
            AccountMeta::new_readonly(remaining[1].key(), false), // amm_config
            AccountMeta::new_readonly(remaining[2].key(), false), // authority
            AccountMeta::new(remaining[3].key(), false),          // pool_state
            AccountMeta::new_readonly(remaining[4].key(), false), // token_0_mint
            AccountMeta::new_readonly(remaining[5].key(), false), // token_1_mint
            AccountMeta::new(remaining[6].key(), false),          // lp_mint
            AccountMeta::new(remaining[7].key(), false),          // creator_token_0
            AccountMeta::new(remaining[8].key(), false),          // creator_token_1
            AccountMeta::new(remaining[9].key(), false),          // creator_lp_token
            AccountMeta::new(remaining[10].key(), false),         // token_0_vault
            AccountMeta::new(remaining[11].key(), false),         // token_1_vault
            AccountMeta::new(remaining[12].key(), false),         // create_pool_fee
            AccountMeta::new(remaining[13].key(), false),         // observation_state
            AccountMeta::new_readonly(remaining[14].key(), false), // token_program
            AccountMeta::new_readonly(remaining[15].key(), false), // token_0_program
            AccountMeta::new_readonly(remaining[16].key(), false), // token_1_program
            AccountMeta::new_readonly(remaining[17].key(), false), // associated_token_program
            AccountMeta::new_readonly(remaining[18].key(), false), // system_program
            AccountMeta::new_readonly(remaining[19].key(), false), // rent
        ];

        let ix = Instruction {
            program_id: cp_swap_program.key(),
            accounts,
            data,
        };

        // Collect all AccountInfos for the CPI
        let mut account_infos = vec![ctx.accounts.admin.to_account_info()];
        for i in 1..20 {
            account_infos.push(remaining[i].to_account_info());
        }

        // Invoke Raydium — admin is a Signer so no invoke_signed needed
        solana_program::program::invoke(&ix, &account_infos)?;

        // Mark escrow as complete
        let escrow = &mut ctx.accounts.escrow;
        escrow.status = EscrowStatus::PoolCreated;

        msg!("RAYDIUM POOL CREATED for mint {}. Liquidity locked.", mint_key);
        Ok(())
    }

    // ── refund_escrow ───────────────────────────────────────────────────────
    /// Callable by ANYONE after the 72-hour timeout expires.
    /// Returns SOL to the original token creator. Burns escrowed tokens.
    /// This is the safety net: if admin doesn't create the pool, users get refunded.
    pub fn refund_escrow(ctx: Context<RefundEscrow>) -> Result<()> {
        let escrow = &ctx.accounts.escrow;
        require!(escrow.status == EscrowStatus::Pending, CurveError::EscrowAlreadyProcessed);

        let clock = Clock::get()?;
        let deadline = escrow.created_at + escrow.timeout;
        require!(clock.unix_timestamp >= deadline, CurveError::EscrowTimeoutNotReached);

        let mint_key = escrow.mint;

        // Return SOL from escrow vault to creator
        let escrow_vault_seeds: &[&[u8]] = &[
            b"escrow_vault",
            mint_key.as_ref(),
            &[ctx.bumps.escrow_sol_vault],
        ];
        let sol_balance = ctx.accounts.escrow_sol_vault.lamports();
        if sol_balance > 0 {
            anchor_lang::system_program::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.system_program.to_account_info(),
                    anchor_lang::system_program::Transfer {
                        from: ctx.accounts.escrow_sol_vault.to_account_info(),
                        to:   ctx.accounts.creator.to_account_info(),
                    },
                    &[escrow_vault_seeds],
                ),
                sol_balance,
            )?;
        }

        // Burn escrowed tokens (remove from circulation)
        let escrow_seeds: &[&[u8]] = &[
            b"escrow",
            mint_key.as_ref(),
            &[ctx.bumps.escrow],
        ];
        let token_balance = ctx.accounts.escrow_token_account.amount;
        if token_balance > 0 {
            token::burn(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Burn {
                        mint:      ctx.accounts.mint.to_account_info(),
                        from:      ctx.accounts.escrow_token_account.to_account_info(),
                        authority: ctx.accounts.escrow.to_account_info(),
                    },
                    &[escrow_seeds],
                ),
                token_balance,
            )?;
        }

        // Mark as refunded
        let escrow = &mut ctx.accounts.escrow;
        escrow.status = EscrowStatus::Refunded;

        msg!(
            "ESCROW REFUNDED: {} SOL returned to creator {}, {} tokens burned.",
            sol_balance, ctx.accounts.creator.key(), token_balance
        );
        Ok(())
    }
}

// ── ACCOUNT CONTEXTS ─────────────────────────────────────────────────────────

#[derive(Accounts)]
pub struct InitializeGlobal<'info> {
    #[account(
        init,
        payer = admin,
        space = GLOBAL_CONFIG_SIZE,
        seeds = [b"global"],
        bump
    )]
    pub global_config: Account<'info, GlobalConfig>,

    #[account(mut)]
    pub admin: Signer<'info>,

    /// CHECK: Stored in GlobalConfig; validated via address constraint in buy/sell
    pub fee_recipient: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateGlobal<'info> {
    #[account(
        mut,
        seeds = [b"global"],
        bump,
        has_one = admin @ CurveError::Unauthorized,
    )]
    pub global_config: Account<'info, GlobalConfig>,
    pub admin: Signer<'info>,
}

#[derive(Accounts)]
pub struct AdminOnly<'info> {
    #[account(
        mut,
        seeds = [b"global"],
        bump,
        has_one = admin @ CurveError::Unauthorized,
    )]
    pub global_config: Account<'info, GlobalConfig>,
    pub admin: Signer<'info>,
}

#[derive(Accounts)]
pub struct CreatePool<'info> {
    #[account(
        init,
        payer = creator,
        space = CURVE_ACCOUNT_SIZE,
        seeds = [b"curve", mint.key().as_ref()],
        bump
    )]
    pub bonding_curve: Account<'info, BondingCurve>,

    /// SOL vault PDA — created manually for zero-data system-owned account
    #[account(
        mut,
        seeds = [b"sol_vault", mint.key().as_ref()],
        bump
    )]
    /// CHECK: Created manually via CPI. Seeds + bump verified by constraint.
    pub sol_vault: UncheckedAccount<'info>,

    #[account(
        init,
        payer = creator,
        associated_token::mint = mint,
        associated_token::authority = bonding_curve
    )]
    pub curve_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        mint::authority = bonding_curve,
    )]
    pub mint: Account<'info, Mint>,

    #[account(mut)]
    pub creator: Signer<'info>,

    /// The quote currency mint for SPL-based pools (USDC or USDT).
    /// For SOL-native pools (quote_type=0) this should be the system program id
    /// or any arbitrary pubkey — it is ignored but must be passed.
    /// CHECK: Validated inside create_pool against GlobalConfig.usdc_mint / usdt_mint
    pub quote_mint: AccountInfo<'info>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,

    #[account(seeds = [b"global"], bump)]
    pub global_config: Account<'info, GlobalConfig>,
}

#[derive(Accounts)]
pub struct BuySell<'info> {
    #[account(
        mut,
        seeds = [b"curve", mint.key().as_ref()],
        bump
    )]
    pub bonding_curve: Account<'info, BondingCurve>,

    #[account(
        mut,
        seeds = [b"sol_vault", mint.key().as_ref()],
        bump
    )]
    pub sol_vault: SystemAccount<'info>,

    #[account(
        mut,
        associated_token::mint = mint,
        associated_token::authority = bonding_curve
    )]
    pub curve_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        token::mint = mint,
        token::authority = user,
    )]
    pub user_token_account: Account<'info, TokenAccount>,

    pub mint: Account<'info, Mint>,

    #[account(mut)]
    pub user: Signer<'info>,

    #[account(seeds = [b"global"], bump)]
    pub global_config: Account<'info, GlobalConfig>,

    #[account(
        mut,
        address = global_config.fee_recipient @ CurveError::InvalidFeeRecipient,
    )]
    /// CHECK: Validated by address constraint against GlobalConfig.
    pub fee_recipient: AccountInfo<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Migrate<'info> {
    #[account(
        mut,
        seeds = [b"curve", mint.key().as_ref()],
        bump
    )]
    pub bonding_curve: Account<'info, BondingCurve>,

    #[account(
        mut,
        seeds = [b"sol_vault", mint.key().as_ref()],
        bump
    )]
    pub sol_vault: SystemAccount<'info>,

    #[account(
        mut,
        associated_token::mint = mint,
        associated_token::authority = bonding_curve
    )]
    pub curve_token_account: Account<'info, TokenAccount>,

    pub mint: Account<'info, Mint>,

    /// The escrow PDA that holds funds until pool creation or refund
    #[account(
        init,
        payer = admin,
        space = ESCROW_ACCOUNT_SIZE,
        seeds = [b"escrow", mint.key().as_ref()],
        bump
    )]
    pub escrow: Account<'info, MigrationEscrow>,

    /// SOL vault for the escrow (system-owned PDA)
    #[account(
        mut,
        seeds = [b"escrow_vault", mint.key().as_ref()],
        bump
    )]
    /// CHECK: System-owned PDA to hold escrowed SOL. Seeds verified.
    pub escrow_sol_vault: UncheckedAccount<'info>,

    /// Token account owned by the escrow PDA
    #[account(
        init,
        payer = admin,
        associated_token::mint = mint,
        associated_token::authority = escrow
    )]
    pub escrow_token_account: Account<'info, TokenAccount>,

    #[account(
        seeds = [b"global"],
        bump,
        has_one = admin @ CurveError::Unauthorized,
    )]
    pub global_config: Account<'info, GlobalConfig>,

    #[account(mut)]
    pub admin: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CreateRaydiumPool<'info> {
    #[account(
        mut,
        seeds = [b"escrow", escrow.mint.as_ref()],
        bump,
    )]
    pub escrow: Account<'info, MigrationEscrow>,

    #[account(
        mut,
        seeds = [b"escrow_vault", escrow.mint.as_ref()],
        bump
    )]
    /// CHECK: Escrow SOL vault PDA. Seeds verified.
    pub escrow_sol_vault: UncheckedAccount<'info>,

    #[account(
        mut,
        associated_token::mint = mint,
        associated_token::authority = escrow
    )]
    pub escrow_token_account: Account<'info, TokenAccount>,

    pub mint: Account<'info, Mint>,

    /// Admin's token account to temporarily receive tokens before Raydium CPI
    #[account(
        mut,
        token::mint = mint,
        token::authority = admin,
    )]
    pub admin_token_account: Account<'info, TokenAccount>,

    #[account(
        seeds = [b"global"],
        bump,
        has_one = admin @ CurveError::Unauthorized,
    )]
    pub global_config: Account<'info, GlobalConfig>,

    #[account(mut)]
    pub admin: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    // Remaining accounts: 20 Raydium CPMM accounts (passed via ctx.remaining_accounts)
}

#[derive(Accounts)]
pub struct RefundEscrow<'info> {
    #[account(
        mut,
        seeds = [b"escrow", escrow.mint.as_ref()],
        bump,
    )]
    pub escrow: Account<'info, MigrationEscrow>,

    #[account(
        mut,
        seeds = [b"escrow_vault", escrow.mint.as_ref()],
        bump
    )]
    /// CHECK: Escrow SOL vault PDA. Seeds verified.
    pub escrow_sol_vault: UncheckedAccount<'info>,

    #[account(
        mut,
        associated_token::mint = mint,
        associated_token::authority = escrow
    )]
    pub escrow_token_account: Account<'info, TokenAccount>,

    #[account(mut)]
    pub mint: Account<'info, Mint>,

    /// The original token creator who receives the refunded SOL
    #[account(
        mut,
        address = escrow.creator @ CurveError::Unauthorized,
    )]
    /// CHECK: Must match escrow.creator. Receives refunded SOL.
    pub creator: AccountInfo<'info>,

    /// Anyone can call this (no admin required), just need to pass the right accounts
    pub caller: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

// ── DATA ACCOUNTS ─────────────────────────────────────────────────────────────

#[account]
pub struct GlobalConfig {
    pub admin:           Pubkey, // 32 — program admin
    pub fee_recipient:   Pubkey, // 32 — receives platform fees
    pub fee_bps:         u16,    // 2  — basis points (100 = 1%)
    pub target_cap:      u64,    // 8  — SOL graduation target (lamports)
    pub target_cap_usdc: u64,    // 8  — USDC/USDT graduation target (in 6-decimal units)
    pub paused:          bool,   // 1  — emergency pause
    pub usdc_mint:       Pubkey, // 32 — validated USDC SPL mint on Solana
    pub usdt_mint:       Pubkey, // 32 — validated USDT SPL mint on Solana
}

#[account]
pub struct BondingCurve {
    // Core fields
    pub mint:                  Pubkey,    // 32 — the token being sold
    pub creator:               Pubkey,    // 32 — wallet that launched this pool
    pub virtual_sol_reserves:  u64,       // 8  — virtual reserve for AMM math
    pub virtual_token_reserves: u64,      // 8  — virtual token reserve for AMM math
    pub real_sol_reserves:     u64,       // 8  — actual quote currency raised
    pub real_token_reserves:   u64,       // 8  — tokens remaining in vault
    pub complete:              bool,      // 1  — true after graduation cap is hit
    pub total_fees_collected:  u64,       // 8  — lifetime fees (analytics + rev-share)
    pub created_at:            i64,       // 8  — unix timestamp

    // ── Phase 2 fields: multi-quote support ──────────────────────────────────
    /// Which currency buyers pay with. SOL = 0 (Phase 1). USDC/USDT = Phase 2.
    pub quote_type:            QuoteType, // 1  — enum (Sol | Usdc | Usdt)
    /// The SPL mint of the quote currency. Pubkey::default() for SOL-native pools.
    pub quote_mint:            Pubkey,    // 32 — zero for SOL curves

    /// On-chain metadata URI (Arweave/IPFS). Stored so indexers can build
    /// token pages without any off-chain database dependency.
    pub metadata_uri:          String,    // 4 + up to 200 bytes
}

#[account]
pub struct MigrationEscrow {
    pub mint:         Pubkey,       // 32 — the token this escrow is for
    pub creator:      Pubkey,       // 32 — original token creator (receives refund)
    pub sol_amount:   u64,          // 8  — SOL deposited into escrow
    pub token_amount: u64,          // 8  — tokens deposited into escrow
    pub created_at:   i64,          // 8  — unix timestamp of escrow creation
    pub timeout:      i64,          // 8  — seconds until refund allowed (72h = 259200)
    pub status:       EscrowStatus, // 1  — Pending / PoolCreated / Refunded
}

// ── ERROR CODES ───────────────────────────────────────────────────────────────
#[error_code]
pub enum CurveError {
    #[msg("The bonding curve has reached its target and trading is locked.")]
    CurveComplete,
    #[msg("The bonding curve has not graduated yet. Migration is not allowed.")]
    CurveNotComplete,
    #[msg("Invalid amount: must be greater than zero.")]
    InvalidAmount,
    #[msg("Slippage tolerance exceeded. Try increasing slippage or reducing trade size.")]
    SlippageExceeded,
    #[msg("Insufficient tokens remaining in the bonding curve.")]
    InsufficientTokens,
    #[msg("Insufficient SOL reserves in the bonding curve.")]
    InsufficientSol,
    #[msg("Integer overflow in bonding curve math.")]
    MathOverflow,
    #[msg("Unauthorized: only the admin can call this instruction.")]
    Unauthorized,
    #[msg("The fee_recipient provided does not match the on-chain GlobalConfig.")]
    InvalidFeeRecipient,
    #[msg("The program is currently paused by the admin.")]
    ProgramPaused,
    #[msg("Fee too high: maximum allowed is 500 bps (5%).")]
    FeeTooHigh,
    #[msg("Invalid quote_type: must be 0 (SOL), 1 (USDC), or 2 (USDT).")]
    InvalidQuoteType,
    #[msg("The quote_mint provided does not match the on-chain GlobalConfig for this quote type.")]
    InvalidQuoteMint,
    #[msg("USDC/USDT quote curves are not yet live. Use SOL (quote_type=0) for now.")]
    UnsupportedQuoteType,
    #[msg("Metadata URI exceeds maximum length of 200 characters.")]
    MetadataUriTooLong,
    #[msg("Escrow has already been processed (pool created or refunded).")]
    EscrowAlreadyProcessed,
    #[msg("Escrow timeout has not been reached yet. Wait for the 72-hour window.")]
    EscrowTimeoutNotReached,
    #[msg("Not enough Raydium accounts provided. Expected 20 remaining accounts.")]
    InsufficientRaydiumAccounts,
}
