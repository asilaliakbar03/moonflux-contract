use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{
        self, Mint, Token, TokenAccount, MintTo, Transfer,
        SetAuthority, spl_token::instruction::AuthorityType,
    },
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
    /// Called by admin bot after graduation. Sends SOL + remaining tokens
    /// to the migration wallet for Raydium LP seeding.
    pub fn migrate(ctx: Context<Migrate>) -> Result<()> {
        let curve = &ctx.accounts.bonding_curve;
        require!(curve.complete, CurveError::CurveNotComplete);

        let sol_balance   = ctx.accounts.sol_vault.lamports();
        let token_balance = ctx.accounts.curve_token_account.amount;
        let mint_key      = ctx.accounts.mint.key();

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
                        to:   ctx.accounts.migration_wallet.to_account_info(),
                    },
                    &[vault_seeds],
                ),
                sol_balance,
            )?;
        }

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
                        to:        ctx.accounts.migration_token_account.to_account_info(),
                        authority: ctx.accounts.bonding_curve.to_account_info(),
                    },
                    &[curve_seeds],
                ),
                token_balance,
            )?;
        }

        msg!(
            "MIGRATE: {} lamports + {} tokens → migration wallet {}",
            sol_balance, token_balance, ctx.accounts.migration_wallet.key()
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

    #[account(mut)]
    /// CHECK: Admin-controlled migration destination
    pub migration_wallet: AccountInfo<'info>,

    #[account(
        mut,
        token::mint = mint,
        token::authority = migration_wallet,
    )]
    pub migration_token_account: Account<'info, TokenAccount>,

    #[account(
        seeds = [b"global"],
        bump,
        has_one = admin @ CurveError::Unauthorized,
    )]
    pub global_config: Account<'info, GlobalConfig>,

    pub admin: Signer<'info>,
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
}
