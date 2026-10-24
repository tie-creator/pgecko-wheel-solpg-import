# KABO Haunted Wheel contract

Development Anchor program for a weighted, VRF-settled SPL-token prize wheel.

The sole fixed administrator is `48Vb2ToJigXko4FmLbq1LCFu4f7JtMLdY7s7nE77mpmr`. Initialization rejects every other signer. There is no instruction to replace or transfer this authority.

## Economy

The production configuration is locked to the PGECKO Token-2022 mint `J4cMCaevryUZcfcu9vyzCkZmPsCfPNdLYkuuW9LYpump` and a 2,000 PGECKO wager (`2,000,000,000` base units). The program rejects initialization with any other mint or stake. Every wager, burn, and payout uses that mint.

The verified mainnet mint has 6 decimals, no mint authority, no freeze authority, and only the `metadataPointer` and `tokenMetadata` extensions. The contract uses Anchor's token interface so its vault, transfers, payouts, and burns are routed through Token-2022.

| Outcome | Probability | Gross outcome | Player receives | Burn |
|---|---:|---:|---:|---:|
| Skull | 15% | 0x | 0x | 20% of wager |
| Ghost | 12% | 0x | 0x | 20% of wager |
| Spider | 8% | 0x | 0x | 20% of wager |
| Candy | 30% | 1.05x | 1.045x | 10% of profit |
| Pumpkin | 20% | 1.20x | 1.18x | 10% of profit |
| Witch | 10% | 1.50x | 1.45x | 10% of profit |
| Vampire | 4% | 2.25x | 2.125x | 10% of profit |
| Jackpot | 1% | 10.50x | 9.55x | 10% of profit |

Long-run theoretical flow is 87.5% returned to players, 9.5% burned, and 3% retained in the prize vault. There is no game treasury and no withdrawal instruction.

## Contract behavior

- `initialize(stake)` creates one game and PDA vault for the configured mint.
- `start_spin(nonce, seed)` transfers the fixed wager, reserves the maximum 10.5x gross liability, and requests ORAO randomness.
- `settle_spin()` is permissionless. It derives the weighted result from fulfilled ORAO bytes, then atomically burns and pays.
- `refund_stalled_spin()` refunds after one hour only while randomness remains unfulfilled.
- `set_paused(bool)` pauses new spins while settlement and refunds remain available.
- `admin_deposit(amount)` lets the configured admin add manually converted wager tokens to the prize vault.
- `recover_non_wager_token(amount)` lets the admin recover accidental or fee tokens from a PDA-controlled token account for manual conversion. It rejects the wager mint, so it cannot withdraw prize-vault assets.
- The vault rejects a new spin unless it covers maximum gross liability for every open spin.
- Direct SPL transfers can replenish the vault. Creator-fee conversion should send its resulting wager tokens directly to this vault account.

An on-chain program cannot wake itself when tokens arrive. The normal design is for the next spin transaction to invoke permissionless fee processing. As a recovery path, the admin can pause, recover non-wager fee tokens, convert them manually, and deposit the wager-token output into the protected prize vault. The admin cannot sign directly as the PDA and cannot use this recovery instruction to remove the configured wager mint.

## Verification

```text
cargo test -p kabo-wheel
python tests/check_economics.py
python tests/bankroll_stress.py
```

The Rust unit tests cover probability totals, weighted boundaries, payout and burn arithmetic. The Python model exhaustively maps all 10,000 weighted buckets, checks the accounting identity, and runs deterministic frequency and bankroll simulations.

Local validator and devnet integration still require Solana/Agave 2.3.0 and Anchor CLI 0.32.1. Official Windows support uses WSL. Generate a fresh program keypair before deployment and replace the placeholder program ID with `anchor keys sync`. `MAINNET_READINESS.md` lists the remaining signing and deployment gates.

This code is for development and independent review. It is not audited or approved for real-money use.
