//! KABO Haunted Wheel v2: weighted 8-outcome SPL Token wagering game.
//! DEVNET / INDEPENDENT AUDIT FIRST: NOT MAINNET READY OR COMPILED HERE.
//! The odds and payout schedule are fixed, immutable and openly disclosed.
//! Probability BPS: [1500,1200,800,3000,2000,1000,400,100].
//! Player NET payout BPS: [0,0,0,10450,11800,14500,21250,95500].
//! 87.5% theoretical NET RTP; 35% loss frequency, 65% profitable frequency.
//! Loss: burn 20% of the wager; the remaining 80% stays in the prize vault.
//! Win: burn 10% of profit; pay the original stake plus 90% of profit.
//! Theoretical 9.5% burn and 3% vault growth. There is no game treasury.
//! Read README before use, especially oracle and solvency limitations.

use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::AssociatedToken,
    token_interface::{self, Burn, Mint, TokenAccount, TokenInterface, TransferChecked},
};
use orao_solana_vrf::{
    program::OraoVrf,
    state::{NetworkState, RandomnessAccountData},
    CONFIG_ACCOUNT_SEED, RANDOMNESS_ACCOUNT_SEED,
};

// PLACEHOLDER: replace with your generated deploy keypair's public key,
// run `anchor keys sync`, and rebuild before deployment.
declare_id!("Fg6PaFpoGXkYsidMpWxqSWYax7pnGUHJWygeYGe4Yc2");

const BPS: u128 = 10_000;
const PROFIT_BURN_BPS: u128 = 1_000; // 10% of profit only
const LOSS_BURN_BPS: u128 = 2_000; // 20% of losing stake
const WEIGHTS_BPS: [u16; 8] = [1_500, 1_200, 800, 3_000, 2_000, 1_000, 400, 100];
const GROSS_PAYOUT_BPS: [u32; 8] = [0, 0, 0, 10_500, 12_000, 15_000, 22_500, 105_000];
const JACKPOT_GROSS_BPS: u32 = 105_000;
const REFUND_AFTER_SECONDS: i64 = 3_600; // ONLY if VRF still pending
const ADMIN_AUTHORITY: Pubkey = pubkey!("48Vb2ToJigXko4FmLbq1LCFu4f7JtMLdY7s7nE77mpmr");
const WAGER_MINT: Pubkey = pubkey!("J4cMCaevryUZcfcu9vyzCkZmPsCfPNdLYkuuW9LYpump");
const STAKE_BASE_UNITS: u64 = 2_000_000_000; // 2,000 PGECKO at 6 decimals

#[program]
pub mod kabo_wheel {
    use super::*;

    /// One game per configured wager mint. Mint, stake, and rules are immutable.
    pub fn initialize(ctx: Context<Initialize>, stake: u64) -> Result<()> {
        require_keys_eq!(
            ctx.accounts.admin.key(),
            ADMIN_AUTHORITY,
            WheelError::UnauthorizedAdmin
        );
        require_keys_eq!(ctx.accounts.mint.key(), WAGER_MINT, WheelError::WrongMint);
        require!(stake == STAKE_BASE_UNITS, WheelError::WrongStake);
        // Ensures even the smallest winning prize and every loss burn base units.
        require!(stake >= 200, WheelError::StakeTooSmall);
        let _ = prize_quote(stake, JACKPOT_GROSS_BPS)?;
        let game = &mut ctx.accounts.game;
        game.admin = ctx.accounts.admin.key();
        game.mint = ctx.accounts.mint.key();
        game.stake = stake;
        game.bump = ctx.bumps.game;
        game.bank_bump = ctx.bumps.bank;
        game.pending_max_gross = 0;
        game.spins_started = 0;
        game.total_burned = 0;
        game.total_paid = 0;
        game.spins_settled = 0;
        game.paused = false;
        Ok(())
    }

    /// One instruction moves KABO to the vault, reserves worst-case liability,
    /// and requests ORAO verified randomness at a user-supplied unique seed.
    /// Player additionally pays the oracle/network fee in SOL.
    pub fn start_spin(ctx: Context<StartSpin>, nonce: u64, seed: [u8; 32]) -> Result<()> {
        require!(seed != [0u8; 32], WheelError::BadSeed);
        require!(!ctx.accounts.game.paused, WheelError::Paused);
        let wager = ctx.accounts.game.stake;
        let (max_gross, _, _) = prize_quote(wager, JACKPOT_GROSS_BPS)?;

        let transfer_accounts = TransferChecked {
            from: ctx.accounts.player_ata.to_account_info(),
            mint: ctx.accounts.mint.to_account_info(),
            to: ctx.accounts.vault.to_account_info(),
            authority: ctx.accounts.player.to_account_info(),
        };
        token_interface::transfer_checked(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                transfer_accounts,
            ),
            wager,
            ctx.accounts.mint.decimals,
        )?;

        // Cover ALL other unsettled rounds plus this round's highest prize.
        ctx.accounts.vault.reload()?;
        let newly_reserved = ctx
            .accounts
            .game
            .pending_max_gross
            .checked_add(max_gross)
            .ok_or(WheelError::Overflow)?;
        // At admission, lock the maximum gross liability for every open spin.
        require!(
            ctx.accounts.vault.amount >= newly_reserved,
            WheelError::InsufficientReserve
        );

        let cpi_accounts = orao_solana_vrf::cpi::accounts::RequestV2 {
            payer: ctx.accounts.player.to_account_info(),
            network_state: ctx.accounts.vrf_config.to_account_info(),
            treasury: ctx.accounts.vrf_treasury.to_account_info(),
            request: ctx.accounts.randomness.to_account_info(),
            system_program: ctx.accounts.system_program.to_account_info(),
        };
        orao_solana_vrf::cpi::request_v2(
            CpiContext::new(ctx.accounts.vrf_program.to_account_info(), cpi_accounts),
            seed,
        )?;

        let spin = &mut ctx.accounts.spin;
        spin.game = ctx.accounts.game.key();
        spin.player = ctx.accounts.player.key();
        spin.player_ata = ctx.accounts.player_ata.key();
        spin.nonce = nonce;
        spin.seed = seed;
        spin.stake = wager;
        spin.max_gross = max_gross;
        spin.started_at = Clock::get()?.unix_timestamp;
        spin.finished = false;
        spin.slot = 255;
        spin.gross = 0;
        spin.paid = 0;
        spin.burned = 0;
        spin.refunded = false;

        let game = &mut ctx.accounts.game;
        game.pending_max_gross = newly_reserved;
        game.spins_started = game
            .spins_started
            .checked_add(1)
            .ok_or(WheelError::Overflow)?;
        emit!(SpinStarted {
            game: game.key(),
            player: spin.player,
            nonce,
            stake: wager,
            seed
        });
        Ok(())
    }

    /// Anyone can settle: outcome is anchored to the completed ORAO VRF account.
    /// No website admin can choose the slot, override it, or avoid a valid burn.
    pub fn settle_spin(ctx: Context<ResolveSpin>) -> Result<()> {
        require!(!ctx.accounts.spin.finished, WheelError::AlreadyFinished);
        let vrf_data = read_vrf(&ctx.accounts.randomness.to_account_info())?;
        let random = vrf_data
            .fulfilled_randomness()
            .ok_or(WheelError::StillPending)?;
        // Fair weighted draw across 10,000 positions using rejection sampling.
        // NEVER use the website's animation to select the outcome.
        let slot = weighted_slot(random)?;
        let wager = ctx.accounts.spin.stake;
        let (gross, paid, burned) = if GROSS_PAYOUT_BPS[slot] == 0 {
            (0, 0, portion(wager, LOSS_BURN_BPS)?)
        } else {
            prize_quote(wager, GROSS_PAYOUT_BPS[slot])?
        };

        // The bank is a PDA; only this program can authorize payouts and burns.
        let game_key = ctx.accounts.game.key();
        let signer_seeds: &[&[u8]] = &[b"bank", game_key.as_ref(), &[ctx.accounts.game.bank_bump]];
        let signers = &[signer_seeds];
        if burned > 0 {
            token_interface::burn(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Burn {
                        mint: ctx.accounts.mint.to_account_info(),
                        from: ctx.accounts.vault.to_account_info(),
                        authority: ctx.accounts.bank.to_account_info(),
                    },
                    signers,
                ),
                burned,
            )?;
        }
        if paid > 0 {
            token_interface::transfer_checked(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    TransferChecked {
                        from: ctx.accounts.vault.to_account_info(),
                        mint: ctx.accounts.mint.to_account_info(),
                        to: ctx.accounts.player_ata.to_account_info(),
                        authority: ctx.accounts.bank.to_account_info(),
                    },
                    signers,
                ),
                paid,
                ctx.accounts.mint.decimals,
            )?;
        }

        let spin = &mut ctx.accounts.spin;
        spin.finished = true;
        spin.slot = slot as u8;
        spin.gross = gross;
        spin.paid = paid;
        spin.burned = burned;

        let game = &mut ctx.accounts.game;
        game.pending_max_gross = game
            .pending_max_gross
            .checked_sub(spin.max_gross)
            .ok_or(WheelError::Overflow)?;
        game.total_burned = game
            .total_burned
            .checked_add(burned as u128)
            .ok_or(WheelError::Overflow)?;
        game.total_paid = game
            .total_paid
            .checked_add(paid as u128)
            .ok_or(WheelError::Overflow)?;
        game.spins_settled = game
            .spins_settled
            .checked_add(1)
            .ok_or(WheelError::Overflow)?;
        emit!(SpinSettled {
            game: game.key(),
            player: spin.player,
            nonce: spin.nonce,
            slot: slot as u8,
            stake: wager,
            gross,
            paid,
            burned,
        });
        Ok(())
    }

    /// Refund full wager ONLY after 1h and only when the oracle is unfulfilled.
    /// A fulfilled but unfavorable outcome can NEVER be canceled/refunded.
    pub fn refund_stalled_spin(ctx: Context<ResolveSpin>) -> Result<()> {
        require!(!ctx.accounts.spin.finished, WheelError::AlreadyFinished);
        let deadline = ctx
            .accounts
            .spin
            .started_at
            .checked_add(REFUND_AFTER_SECONDS)
            .ok_or(WheelError::Overflow)?;
        require!(
            Clock::get()?.unix_timestamp >= deadline,
            WheelError::NotExpired
        );
        let vrf_data = read_vrf(&ctx.accounts.randomness.to_account_info())?;
        require!(
            vrf_data.fulfilled_randomness().is_none(),
            WheelError::AlreadyFulfilled
        );

        let game_key = ctx.accounts.game.key();
        let signer_seeds: &[&[u8]] = &[b"bank", game_key.as_ref(), &[ctx.accounts.game.bank_bump]];
        token_interface::transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                TransferChecked {
                    from: ctx.accounts.vault.to_account_info(),
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.player_ata.to_account_info(),
                    authority: ctx.accounts.bank.to_account_info(),
                },
                &[signer_seeds],
            ),
            ctx.accounts.spin.stake,
            ctx.accounts.mint.decimals,
        )?;
        let spin = &mut ctx.accounts.spin;
        spin.finished = true;
        spin.refunded = true;
        spin.paid = spin.stake;
        let game = &mut ctx.accounts.game;
        game.pending_max_gross = game
            .pending_max_gross
            .checked_sub(spin.max_gross)
            .ok_or(WheelError::Overflow)?;
        emit!(SpinRefunded {
            game: game.key(),
            player: spin.player,
            nonce: spin.nonce,
            amount: spin.stake
        });
        Ok(())
    }

    /// Admin can pause NEW spins only; settlements and stalled refunds remain possible.
    pub fn set_paused(ctx: Context<AdminGame>, paused: bool) -> Result<()> {
        ctx.accounts.game.paused = paused;
        Ok(())
    }

    /// Admin-signed top-up after an off-chain/manual conversion. This can only
    /// move the configured wager mint into the PDA prize vault.
    pub fn admin_deposit(ctx: Context<AdminDeposit>, amount: u64) -> Result<()> {
        require!(amount > 0, WheelError::ZeroAmount);
        token_interface::transfer_checked(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                TransferChecked {
                    from: ctx.accounts.admin_ata.to_account_info(),
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.vault.to_account_info(),
                    authority: ctx.accounts.admin.to_account_info(),
                },
            ),
            amount,
            ctx.accounts.mint.decimals,
        )?;
        emit!(AdminDepositEvent {
            game: ctx.accounts.game.key(),
            admin: ctx.accounts.admin.key(),
            amount,
        });
        Ok(())
    }

    /// Recover an accidentally received non-wager SPL token to the admin ATA
    /// for manual conversion. The configured wager mint is explicitly blocked,
    /// so this instruction cannot remove prize-bank assets.
    pub fn recover_non_wager_token(ctx: Context<RecoverNonWagerToken>, amount: u64) -> Result<()> {
        require!(amount > 0, WheelError::ZeroAmount);
        require_keys_neq!(
            ctx.accounts.source_mint.key(),
            ctx.accounts.game.mint,
            WheelError::CannotRecoverWagerMint
        );
        let game_key = ctx.accounts.game.key();
        let signer_seeds: &[&[u8]] = &[b"bank", game_key.as_ref(), &[ctx.accounts.game.bank_bump]];
        token_interface::transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                TransferChecked {
                    from: ctx.accounts.source_ata.to_account_info(),
                    mint: ctx.accounts.source_mint.to_account_info(),
                    to: ctx.accounts.admin_destination_ata.to_account_info(),
                    authority: ctx.accounts.bank.to_account_info(),
                },
                &[signer_seeds],
            ),
            amount,
            ctx.accounts.source_mint.decimals,
        )?;
        emit!(NonWagerTokenRecovered {
            game: ctx.accounts.game.key(),
            admin: ctx.accounts.admin.key(),
            mint: ctx.accounts.source_mint.key(),
            amount,
        });
        Ok(())
    }
}

fn read_vrf(info: &AccountInfo) -> Result<RandomnessAccountData> {
    require_keys_eq!(*info.owner, orao_solana_vrf::ID, WheelError::BadOracle);
    let data = info.try_borrow_data()?;
    let result = RandomnessAccountData::try_deserialize(&mut &data[..])?;
    Ok(result)
}

/// Uniform draw from the VRF bytes without modulo bias (2^64 rejection).
/// Rejects with negligible probability if all four independent chunks are in
/// the tiny incomplete upper range. A complete production audit must analyze
/// this rare oracle-result liveness case.
fn weighted_slot(random: &[u8]) -> Result<usize> {
    let bound: u128 = 10_000;
    let limit: u128 = ((u64::MAX as u128 + 1) / bound) * bound;
    for chunk in random.chunks_exact(8).take(4) {
        let array: [u8; 8] = chunk.try_into().map_err(|_| WheelError::BadRandomness)?;
        let sample = u64::from_le_bytes(array) as u128;
        if sample < limit {
            let draw = (sample % bound) as u16;
            let mut cumulative: u16 = 0;
            for (i, weight) in WEIGHTS_BPS.iter().enumerate() {
                cumulative = cumulative
                    .checked_add(*weight)
                    .ok_or(WheelError::Overflow)?;
                if draw < cumulative {
                    return Ok(i);
                }
            }
        }
    }
    err!(WheelError::BadRandomness)
}

fn portion(amount: u64, bps: u128) -> Result<u64> {
    let part = (amount as u128)
        .checked_mul(bps)
        .ok_or(WheelError::Overflow)?
        / BPS;
    u64::try_from(part).map_err(|_| WheelError::Overflow.into())
}

/// Quote gross payout, player payment, and the 10% burn on profit only.
fn prize_quote(stake: u64, gross_multiplier_bps: u32) -> Result<(u64, u64, u64)> {
    require!(
        gross_multiplier_bps >= BPS as u32,
        WheelError::PayoutMismatch
    );
    let gross = (stake as u128)
        .checked_mul(gross_multiplier_bps as u128)
        .ok_or(WheelError::Overflow)?
        / BPS;
    let profit = gross
        .checked_sub(stake as u128)
        .ok_or(WheelError::Overflow)?;
    let burned = profit
        .checked_mul(PROFIT_BURN_BPS)
        .ok_or(WheelError::Overflow)?
        / BPS;
    let paid = gross.checked_sub(burned).ok_or(WheelError::Overflow)?;
    Ok((
        u64::try_from(gross).map_err(|_| WheelError::Overflow)?,
        u64::try_from(paid).map_err(|_| WheelError::Overflow)?,
        u64::try_from(burned).map_err(|_| WheelError::Overflow)?,
    ))
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    pub mint: InterfaceAccount<'info, Mint>,
    #[account(init, payer = admin, space = 8 + Game::INIT_SPACE,
        seeds = [b"game", mint.key().as_ref()], bump)]
    pub game: Account<'info, Game>,
    /// CHECK: PDA is an authority only; no account data is required.
    #[account(seeds = [b"bank", game.key().as_ref()], bump)]
    pub bank: UncheckedAccount<'info>,
    #[account(init, payer = admin,
        associated_token::mint = mint, associated_token::authority = bank,
        associated_token::token_program = token_program)]
    pub vault: InterfaceAccount<'info, TokenAccount>,
    pub token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(nonce: u64, seed: [u8; 32])]
pub struct StartSpin<'info> {
    #[account(mut)]
    pub player: Signer<'info>,
    #[account(mut, seeds = [b"game", game.mint.as_ref()], bump = game.bump)]
    pub game: Account<'info, Game>,
    #[account(address = game.mint)]
    pub mint: InterfaceAccount<'info, Mint>,
    #[account(mut, associated_token::mint = mint, associated_token::authority = player,
        associated_token::token_program = token_program)]
    pub player_ata: InterfaceAccount<'info, TokenAccount>,
    /// CHECK: Verified using seeds and bump.
    #[account(seeds = [b"bank", game.key().as_ref()], bump = game.bank_bump)]
    pub bank: UncheckedAccount<'info>,
    #[account(mut, associated_token::mint = mint, associated_token::authority = bank,
        associated_token::token_program = token_program)]
    pub vault: InterfaceAccount<'info, TokenAccount>,
    #[account(init, payer = player, space = 8 + Spin::INIT_SPACE,
        seeds = [b"spin", game.key().as_ref(), player.key().as_ref(), &nonce.to_le_bytes()], bump)]
    pub spin: Account<'info, Spin>,
    /// CHECK: Address derived using ORAO's seeds; ORAO verifies and initializes this account.
    #[account(mut, seeds = [RANDOMNESS_ACCOUNT_SEED, seed.as_ref()], bump,
        seeds::program = orao_solana_vrf::ID)]
    pub randomness: UncheckedAccount<'info>,
    #[account(mut, seeds = [CONFIG_ACCOUNT_SEED], bump,
        seeds::program = orao_solana_vrf::ID)]
    pub vrf_config: Account<'info, NetworkState>,
    /// CHECK: Tied to verified ORAO network state's treasury.
    #[account(mut, address = vrf_config.config.treasury)]
    pub vrf_treasury: UncheckedAccount<'info>,
    pub vrf_program: Program<'info, OraoVrf>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ResolveSpin<'info> {
    #[account(mut)]
    pub caller: Signer<'info>,
    #[account(mut, seeds = [b"game", game.mint.as_ref()], bump = game.bump)]
    pub game: Account<'info, Game>,
    #[account(mut, has_one = game,
        seeds = [b"spin", game.key().as_ref(), spin.player.as_ref(), &spin.nonce.to_le_bytes()], bump)]
    pub spin: Account<'info, Spin>,
    /// CHECK: Token recipient address is forced to be this saved wallet's ATA.
    #[account(address = spin.player)]
    pub player: UncheckedAccount<'info>,
    #[account(mut, address = game.mint)]
    pub mint: InterfaceAccount<'info, Mint>,
    #[account(init_if_needed, payer = caller,
        associated_token::mint = mint, associated_token::authority = player,
        associated_token::token_program = token_program)]
    pub player_ata: InterfaceAccount<'info, TokenAccount>,
    /// CHECK: Verified by seeds and bump.
    #[account(seeds = [b"bank", game.key().as_ref()], bump = game.bank_bump)]
    pub bank: UncheckedAccount<'info>,
    #[account(mut, associated_token::mint = mint, associated_token::authority = bank,
        associated_token::token_program = token_program)]
    pub vault: InterfaceAccount<'info, TokenAccount>,
    /// CHECK: Verified ORAO program owner, seeds and decoded account data.
    #[account(owner = orao_solana_vrf::ID,
        seeds = [RANDOMNESS_ACCOUNT_SEED, spin.seed.as_ref()], bump,
        seeds::program = orao_solana_vrf::ID)]
    pub randomness: UncheckedAccount<'info>,
    pub token_program: Interface<'info, TokenInterface>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct AdminGame<'info> {
    pub admin: Signer<'info>,
    #[account(mut, has_one = admin,
        seeds = [b"game", game.mint.as_ref()], bump = game.bump)]
    pub game: Account<'info, Game>,
}

#[derive(Accounts)]
pub struct AdminDeposit<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(has_one = admin, seeds = [b"game", game.mint.as_ref()], bump = game.bump)]
    pub game: Account<'info, Game>,
    #[account(address = game.mint)]
    pub mint: InterfaceAccount<'info, Mint>,
    #[account(mut, associated_token::mint = mint, associated_token::authority = admin,
        associated_token::token_program = token_program)]
    pub admin_ata: InterfaceAccount<'info, TokenAccount>,
    /// CHECK: PDA authority verified by seeds and stored bump.
    #[account(seeds = [b"bank", game.key().as_ref()], bump = game.bank_bump)]
    pub bank: UncheckedAccount<'info>,
    #[account(mut, associated_token::mint = mint, associated_token::authority = bank,
        associated_token::token_program = token_program)]
    pub vault: InterfaceAccount<'info, TokenAccount>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct RecoverNonWagerToken<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(has_one = admin, seeds = [b"game", game.mint.as_ref()], bump = game.bump)]
    pub game: Account<'info, Game>,
    pub source_mint: InterfaceAccount<'info, Mint>,
    /// CHECK: PDA authority verified by seeds and stored bump.
    #[account(seeds = [b"bank", game.key().as_ref()], bump = game.bank_bump)]
    pub bank: UncheckedAccount<'info>,
    #[account(mut, token::mint = source_mint, token::authority = bank,
        token::token_program = token_program)]
    pub source_ata: InterfaceAccount<'info, TokenAccount>,
    #[account(mut, associated_token::mint = source_mint, associated_token::authority = admin,
        associated_token::token_program = token_program)]
    pub admin_destination_ata: InterfaceAccount<'info, TokenAccount>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[account]
#[derive(InitSpace)]
pub struct Game {
    pub admin: Pubkey,
    pub mint: Pubkey,
    pub stake: u64,
    pub bump: u8,
    pub bank_bump: u8,
    pub pending_max_gross: u64,
    pub spins_started: u64,
    pub total_burned: u128,
    pub total_paid: u128,
    pub spins_settled: u64,
    pub paused: bool,
}

#[account]
#[derive(InitSpace)]
pub struct Spin {
    pub game: Pubkey,
    pub player: Pubkey,
    pub player_ata: Pubkey,
    pub nonce: u64,
    pub seed: [u8; 32],
    pub stake: u64,
    pub max_gross: u64,
    pub started_at: i64,
    pub finished: bool,
    pub slot: u8,
    pub gross: u64,
    pub paid: u64,
    pub burned: u64,
    pub refunded: bool,
}

#[event]
pub struct SpinStarted {
    pub game: Pubkey,
    pub player: Pubkey,
    pub nonce: u64,
    pub stake: u64,
    pub seed: [u8; 32],
}
#[event]
pub struct SpinSettled {
    pub game: Pubkey,
    pub player: Pubkey,
    pub nonce: u64,
    pub slot: u8,
    pub stake: u64,
    pub gross: u64,
    pub paid: u64,
    pub burned: u64,
}
#[event]
pub struct SpinRefunded {
    pub game: Pubkey,
    pub player: Pubkey,
    pub nonce: u64,
    pub amount: u64,
}

#[event]
pub struct AdminDepositEvent {
    pub game: Pubkey,
    pub admin: Pubkey,
    pub amount: u64,
}

#[event]
pub struct NonWagerTokenRecovered {
    pub game: Pubkey,
    pub admin: Pubkey,
    pub mint: Pubkey,
    pub amount: u64,
}

#[error_code]
pub enum WheelError {
    #[msg("Stake too small for nonzero burns; use at least 200 base units")]
    StakeTooSmall,
    #[msg("Seed cannot be zero; use a fresh random seed")]
    BadSeed,
    #[msg("Game is paused")]
    Paused,
    #[msg("Prize vault cannot guarantee the highest outcome for all open spins")]
    InsufficientReserve,
    #[msg("Arithmetic overflow or underflow")]
    Overflow,
    #[msg("Spin has already been resolved")]
    AlreadyFinished,
    #[msg("Randomness is still pending")]
    StillPending,
    #[msg("Oracle account is not owned by ORAO")]
    BadOracle,
    #[msg("Stalled spin timeout has not passed")]
    NotExpired,
    #[msg("Randomness already fulfilled: result must be settled, not refunded")]
    AlreadyFulfilled,
    #[msg("VRF random sample cannot be mapped without modulo bias")]
    BadRandomness,
    #[msg("Gross prize rounding failed to preserve advertised net payout")]
    PayoutMismatch,
    #[msg("Amount must be greater than zero")]
    ZeroAmount,
    #[msg("Prize-vault tokens cannot be recovered through the non-wager rescue path")]
    CannotRecoverWagerMint,
    #[msg("Only the fixed KABO administrator may initialize or administer this game")]
    UnauthorizedAdmin,
    #[msg("This deployment only accepts the configured PGECKO mint")]
    WrongMint,
    #[msg("This deployment requires the fixed 2,000-token wager")]
    WrongStake,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_and_net_rtp_are_exact() {
        assert_eq!(WEIGHTS_BPS.iter().map(|x| *x as u32).sum::<u32>(), 10_000);
        let mut rtp_numerator: u64 = 0;
        let mut lose_weight: u64 = 0;
        for (weight, mult) in WEIGHTS_BPS.iter().zip(GROSS_PAYOUT_BPS.iter()) {
            rtp_numerator += (*weight as u64) * (*mult as u64);
            if *mult == 0 {
                lose_weight += *weight as u64;
            }
        }
        assert_eq!(rtp_numerator, 90_000_000); // 90% gross before profit burns
        assert_eq!(lose_weight, 3_500); // 35% zero payout
    }

    #[test]
    fn loss_burn() {
        assert_eq!(portion(10_000, LOSS_BURN_BPS).unwrap(), 2_000);
    }

    #[test]
    fn gross_prize_rounds_to_exact_net() {
        let stake = 10_000;
        assert_eq!(prize_quote(stake, 10_500).unwrap(), (10_500, 10_450, 50));
        assert_eq!(
            prize_quote(stake, 105_000).unwrap(),
            (105_000, 95_500, 9_500)
        );
        for stake in [25_u64, 26, 101, 1011, 1_000_000, 1_000_000_000] {
            for mult in GROSS_PAYOUT_BPS.iter().filter(|m| **m > 0) {
                let (gross, paid, burned) = prize_quote(stake, *mult).unwrap();
                assert_eq!(gross, paid + burned);
                assert_eq!(gross, (stake as u128 * (*mult as u128) / BPS) as u64);
            }
        }
    }

    #[test]
    fn weighted_picker_boundary() {
        let samples: [(u64, usize); 8] = [
            (0, 0),
            (1_500, 1),
            (2_700, 2),
            (3_500, 3),
            (6_500, 4),
            (8_500, 5),
            (9_500, 6),
            (9_900, 7),
        ];
        for (draw, slot) in samples {
            let mut bytes = [0u8; 32];
            bytes[0..8].copy_from_slice(&draw.to_le_bytes());
            assert_eq!(weighted_slot(&bytes).unwrap(), slot);
        }
    }
}
