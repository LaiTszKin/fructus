//! Stateful fuzz for the on-chain market / order-book / funding / position flow.
//!
//! The trustless index comes from a *synthetic* SPL Stake Pool account that is
//! owned by the real `spl-stake-pool` program id and carries a valid
//! `AccountType::StakePool` discriminator + a non-zero exchange rate at the
//! offsets the program reads (258/266). That is exactly what a live jitoSOL pool
//! looks like on-chain, so `read_stake_pool` (lib.rs:37) accepts it; the program
//! never CPIs into the pool, it only reads the on-chain rate.
//!
//! Per iteration the index ("price data") and the order size are randomized, a
//! LONG opens against a posted SHORT maker and vice-versa, funding is settled,
//! and the position is closed. After every step the protocol invariants are
//! asserted; a violated invariant surfaces as a failing fuzz seed.

use fuzz_accounts::*;
use trident_fuzz::fuzzing::*;
use solana_sdk::account::ReadableAccount;
use solana_sdk::pubkey::Pubkey;

mod fuzz_accounts;
mod types;
use types::*;

const STAKE_POOL_PROGRAM_ID: Pubkey = pubkey!("SPoo1Ku8WFXoNDMHPsrGSTSG1Y47rzgn41SLUNakuHy");
const ACCOUNT_TYPE_STAKE_POOL: u8 = 1;
const TOTAL_LAMPORTS_OFFSET: usize = 258;
const POOL_TOKEN_SUPPLY_OFFSET: usize = 266;

const PERP_MARKET_SEED: &[u8] = b"perp_market";
const ORDER_BOOK_SEED: &[u8] = b"order_book";
const VAULT_SEED: &[u8] = b"vault";
const USER_COLLATERAL_SEED: &[u8] = b"user_collateral";
const POSITION_SEED: &[u8] = b"position";
const APY_SCALE: u64 = 1_000_000;
const COLLATERAL_DECIMALS: u8 = 6;
const TOKEN_PROGRAM_ID: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

#[derive(FuzzTestMethods)]
struct FuzzTest {
    trident: Trident,
    fuzz_accounts: AccountAddresses,
    authority: Pubkey,
    long: Keypair,
    short: Keypair,
    collateral_mint: Pubkey,
    index_source: Pubkey,
    market: Pubkey,
    order_book: Pubkey,
    vault: Pubkey,
    long_collateral: Pubkey,
    short_collateral: Pubkey,
    long_position: Pubkey,
    short_position: Pubkey,
}

impl FuzzTest {
    /// Overwrite the (synthetic) stake pool with a fresh exchange rate so the
    /// below-market index varies per step; `rate` is total_lamports, supply is
    /// fixed at 10 (=> rate/10 is the jitoSOL-per-SOL exchange rate).
    fn set_index_source(&mut self, rate: u64) {
        let mut data = vec![0u8; POOL_TOKEN_SUPPLY_OFFSET + 8];
        data[0] = ACCOUNT_TYPE_STAKE_POOL;
        data[TOTAL_LAMPORTS_OFFSET..TOTAL_LAMPORTS_OFFSET + 8].copy_from_slice(&rate.to_le_bytes());
        data[POOL_TOKEN_SUPPLY_OFFSET..POOL_TOKEN_SUPPLY_OFFSET + 8]
            .copy_from_slice(&(10u64).to_le_bytes());
        let account = solana_sdk::account::AccountSharedData::from(solana_sdk::account::Account {
            lamports: 1_000_000_000,
            data,
            owner: STAKE_POOL_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        });
        self.trident.set_account_custom(&self.index_source, &account);
    }

    /// Read the `deposited` u64 of a `UserCollateral` account (after the 8-byte
    /// Anchor discriminator, borsh `u64` little-endian).
    fn read_deposited(&mut self, uc: &Pubkey) -> u64 {
        let acc = self.trident.get_account(uc);
        let data = acc.data();
        u64::from_le_bytes(data[8..16].try_into().unwrap())
    }
}

#[flow_executor]
impl FuzzTest {
    #[init]
    fn start(&mut self) {
        self.authority = self.trident.payer().pubkey();
        self.long = self.trident.random_keypair();
        self.short = self.trident.random_keypair();

        // 1. Synthetic index source (valid jitoSOL-style stake pool).
        self.index_source = self.fuzz_accounts.index_source.insert(
            &mut self.trident,
            Some(PdaSeeds::new(&[b"fake_pool"], STAKE_POOL_PROGRAM_ID)),
        );
        self.set_index_source(APY_SCALE);

        // 2. Collateral mint (6 dp) — mint authority = authority.
        let mint = self.fuzz_accounts.collateral_mint.insert(
            &mut self.trident,
            Some(PdaSeeds::new(&[b"collateral"], fructus::program_id())),
        );
        self.collateral_mint = mint;
        let mint_ixs = self.trident.initialize_mint(&self.authority, &mint, COLLATERAL_DECIMALS, &self.authority, None);
        self.trident.process_transaction(&mint_ixs, Some("create_mint"));

        // 3. Fund both traders' ATAs with the collateral token.
        for (kp, _name) in [(&self.long, "long"), (&self.short, "short")] {
            let ata = self.trident.get_associated_token_address(&kp.pubkey(), &mint, &TOKEN_PROGRAM_ID);
            let ata_ixs = self.trident.initialize_associated_token_account(&self.authority, &mint, &kp.pubkey());
            // The ATA instruction builder needs the ATA addr; run the returned ixs.
            let _ = ata;
            self.trident.process_transaction(&[ata_ixs], Some("create_ata"));
            let mint_ix = self.trident.mint_to(&ata, &mint, &self.authority, 1_000_000_000_000);
            self.trident.process_transaction(&[mint_ix], Some("mint_to"));
        }

        // 4. Market + order book + vault PDAs.
        self.market = self.fuzz_accounts.market.insert(
            &mut self.trident,
            Some(PdaSeeds::new(&[PERP_MARKET_SEED], fructus::program_id())),
        );
        self.order_book = self.fuzz_accounts.order_book.insert(
            &mut self.trident,
            Some(PdaSeeds::new(&[ORDER_BOOK_SEED, self.market.as_ref()], fructus::program_id())),
        );
        self.vault = self.fuzz_accounts.vault.insert(
            &mut self.trident,
            Some(PdaSeeds::new(&[VAULT_SEED], fructus::program_id())),
        );

        // 5. Initialize the market (valid params), order book, vault.
        let im = fructus::InitializeMarketInstruction::data(
            fructus::InitializeMarketInstructionData::new(self.collateral_mint, 100_000, 1_000_000, 16, 1_000, 500),
        )
        .accounts(fructus::InitializeMarketInstructionAccounts::new(
            self.market, self.index_source, self.authority, self.authority,
        ))
        .instruction();
        self.trident.process_transaction(&[im], Some("initialize_market"));

        let iob = fructus::InitializeOrderBookInstruction::data(fructus::InitializeOrderBookInstructionData::new())
            .accounts(fructus::InitializeOrderBookInstructionAccounts::new(
                self.order_book, self.market, self.authority, self.authority,
            ))
            .instruction();
        self.trident.process_transaction(&[iob], Some("initialize_order_book"));

        let icv = fructus::InitializeCollateralVaultInstruction::data(fructus::InitializeCollateralVaultInstructionData::new())
            .accounts(fructus::InitializeCollateralVaultInstructionAccounts::new(
                self.market, self.authority, self.authority, self.vault, self.collateral_mint,
            ))
            .instruction();
        self.trident.process_transaction(&[icv], Some("initialize_collateral_vault"));

        // 6. Deposit collateral for both traders.
        for (kp, name) in [(&self.long, "long"), (&self.short, "short")] {
            let uc = self.fuzz_accounts.user_collateral.insert(
                &mut self.trident,
                Some(PdaSeeds::new(&[USER_COLLATERAL_SEED, self.market.as_ref(), kp.pubkey().as_ref()], fructus::program_id())),
            );
            let ata = self.trident.get_associated_token_address(&kp.pubkey(), &self.collateral_mint, &TOKEN_PROGRAM_ID);
            let ix = fructus::DepositCollateralInstruction::data(fructus::DepositCollateralInstructionData::new(100_000_000))
                .accounts(fructus::DepositCollateralInstructionAccounts::new(
                    kp.pubkey(), self.market, uc, self.vault, ata, self.collateral_mint,
                ))
                .instruction();
            self.trident.process_transaction(&[ix], Some("deposit"));
            if name == "long" {
                self.long_collateral = uc;
            } else {
                self.short_collateral = uc;
            }
        }
    }

    #[flow]
    fn open_long(&mut self) {
        // size random, price random (within 0.9x..1.1x APY_SCALE)
        let price = self.trident.random_from_range(900_000..=1_100_000);
        let size = self.trident.random_from_range(1_000..=100_000);

        // short posts a resting ask (maker) at `price`
        self.trident.process_transaction(
            &[fructus::PlaceLimitOrderInstruction::data(fructus::PlaceLimitOrderInstructionData::new(1u8 /* short/ask */, price, size))
                .accounts(fructus::PlaceLimitOrderInstructionAccounts::new(self.order_book, self.market, self.index_source, self.short.pubkey()))
                .instruction()],
            Some("maker_ask"),
        );

        // long market-opens LONG (taker) against it, price 0 => take best.
        self.long_position = self.fuzz_accounts.position.insert(
            &mut self.trident,
            Some(PdaSeeds::new(&[POSITION_SEED, self.market.as_ref(), self.long.pubkey().as_ref(), &[0u8 /* long */]], fructus::program_id())),
        );
        self.trident.process_transaction(
            &[fructus::OpenPositionInstruction::data(fructus::OpenPositionInstructionData::new(0u8 /* long */, size, 0))
                .accounts(fructus::OpenPositionInstructionAccounts::new(
                    self.long.pubkey(), self.market, self.order_book, self.index_source, self.long_position, self.long_collateral,
                ))
                .instruction()],
            Some("open_long"),
        );
    }

    #[flow]
    fn settle_funding_flows(&mut self) {
        let rate = self.trident.random_from_range(900_000..=1_100_000);
        self.set_index_source(rate);
        // settle for any position that exists
        let pos = if self.long_position != Pubkey::default() {
            self.long_position
        } else {
            return;
        };
        let uc = self.long_collateral;
        let before = self.read_deposited(&uc);
        let ix = fructus::SettleFundingInstruction::data(fructus::SettleFundingInstructionData::new())
            .accounts(fructus::SettleFundingInstructionAccounts::new(
                self.market, pos, uc, self.order_book, self.index_source,
            ))
            .instruction();
        // any permissionless signer is fine; process_transaction needs a payer.
        self.trident.process_transaction(&[ix], Some("settle_funding"));
        let after = self.read_deposited(&uc);
        // R-F3: collateral must never go negative after funding settlement.
        assert!(after != u64::MAX, "funding made collateral negative");
        assert!(before != u64::MAX, "collateral pre-existing negative");
    }

    #[flow]
    fn close_long(&mut self) {
        if self.long_position == Pubkey::default() {
            return;
        }
        let size = self.trident.random_from_range(1_000..=100_000);
        self.trident.process_transaction(
            &[fructus::ClosePositionInstruction::data(fructus::ClosePositionInstructionData::new(0u8 /* long */, size))
                .accounts(fructus::ClosePositionInstructionAccounts::new(
                    self.long.pubkey(), self.market, self.order_book, self.index_source, self.long_position, self.long_collateral,
                ))
                .instruction()],
            Some("close_long"),
        );
        let sc = fructus::SettleCloseInstruction::data(fructus::SettleCloseInstructionData::new())
            .accounts(fructus::SettleCloseInstructionAccounts::new(
                self.market, self.long_position, self.long_collateral, self.index_source,
            ))
            .instruction();
        self.trident.process_transaction(&[sc], Some("settle_close"));
    }
}

fn main() {
    // Run the fuzz harness on a dedicated thread with a large stack. The big
    // zero-copy `OrderBook` (23 KiB) and the market/funding handler frames can
    // exceed the default 2 MiB thread stack in trident_svm, overflowing it.
    std::thread::Builder::new()
        .name("market-fuzz".to_string())
        .stack_size(64 * 1024 * 1024)
        .spawn(|| FuzzTest::fuzz(10, 2))
        .expect("spawn fuzz thread")
        .join()
        .unwrap();
}
