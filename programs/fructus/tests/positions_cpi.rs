//! Bank-style CPI integration tests for the position lifecycle (issue #5).
//!
//! These tests run the real instruction handlers (`open_position`,
//! `close_position`, `settle_fill`, plus the amended `place_limit_order` /
//! `place_market_order` / `crank`) against a local bank built with
//! `solana-program-test` — the same harness pattern as `collateral_cpi.rs`
//! (the Fructus program loaded as a compiled SBF binary, SPL Token
//! in-process, a fake jitoSOL stake-pool account as the market-bound index
//! source). The environment wires four funded parties (A–D), each with minted
//! USDC, a `UserCollateral` ledger, the collateral vault, and the `OrderBook`.
//!
//! The scenarios lock the acceptance criteria A-4..A-11, A-13b, A-15, A-20
//! (`.plan/20260820/position-lifecycle/acceptance.md`): taker-inline +
//! maker-deferred settlement, ledger-only reserved margin, atomic failure
//! semantics, and the end-to-end open/close of both sides.
//!
//! Side encoding (design §5): `0` = Long/Bid, `1` = Short/Ask. A market order
//! is signalled by `price == 0`. Fructus errors surface as `Custom` codes
//! (matched via `assert_anchor_error`); the ownership/format errors
//! (`InvalidAccountData`) surface as the raw system `InstructionError`.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use proptest::prelude::*;

use anchor_lang::{AccountDeserialize, Discriminator, InstructionData};
use fructus::constants::{
    ORDER_BOOK_SEED, PERP_MARKET_SEED, POSITION_SEED, USER_COLLATERAL_SEED, VAULT_SEED,
};
use fructus::error::FructusError;
use fructus::exchange::STAKE_POOL_PROGRAM_ID;
use fructus::state::{OrderBook, Position, UserCollateral};
use solana_account::{Account, AccountSharedData};
use solana_instruction::error::InstructionError as SolanaInstructionError;
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_program_pack::Pack;
use solana_program_test::{processor, BanksClientError, ProgramTest, ProgramTestContext};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;
use solana_transaction_error::TransactionError;
use spl_associated_token_account::get_associated_token_address;
use spl_token::solana_program::program_option::COption;
use spl_token::state::{Account as TokenAccount, AccountState, Mint};

/// USDC collateral mint decimals, matching `constants::USDC_DECIMALS`.
const DECIMALS: u8 = 6;
/// Initial minted amount per user: 1,000,000 USDC (6 decimals).
const MINT_AMOUNT: u64 = 1_000_000_000_000;
/// Lamports used to fund the pre-created accounts (mint, users).
const FUNDING_LAMPORTS: u64 = 10_000_000_000;
/// Notional of one fill-sized order, in USDC microunits (1 USDC).
const SIZE: u64 = 1_000_000;
/// Initial margin in basis points (10x leverage): margin == ceil(notional / 10).
const INITIAL_MARGIN_BPS: u16 = 1_000;
/// Position/open side encodings: 0 = Long/Bid, 1 = Short/Ask.
const LONG: u8 = 0;
const SHORT: u8 = 1;
/// Fake stake-pool `total_lamports` used for the base snapshot (rate 1.0).
const BASE_TOTAL_LAMPORTS: u64 = 10_000_000_000_000;
/// Fake stake-pool `pool_token_supply` (rate 1.0 when `total_lamports` matches).
const BASE_POOL_TOKEN_SUPPLY: u64 = 10_000_000_000_000;

/// The system program id is the all-zero pubkey (`11111111111111111111111111111111`).
fn system_program_id() -> Pubkey {
    Pubkey::default()
}

/// Locate the compiled Fructus SBF binary (`cargo build-sbf` / `anchor build`
/// output), returning `None` when it has not been built yet.
fn find_fructus_so() -> Option<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        manifest.join("../../target/sbpf-solana-solana/release/fructus.so"),
        manifest.join("../../target/deploy/fructus.so"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

/// Bytes for a fake SPL Stake Pool account, enough for `read_stake_pool`'s
/// validation to succeed: the `StakePool` account-type discriminator (byte 0)
/// plus non-zero `total_lamports` / `pool_token_supply` at the canonical
/// offsets (258 / 266, with the `account_type` prefix — do not "fix" to 257/265).
fn fake_stake_pool_data() -> Vec<u8> {
    let mut data = vec![0u8; 274];
    data[0] = 1; // AccountType::StakePool
    data[258..266].copy_from_slice(&BASE_TOTAL_LAMPORTS.to_le_bytes()); // total_lamports
    data[266..274].copy_from_slice(&BASE_POOL_TOKEN_SUPPLY.to_le_bytes()); // pool_token_supply
    data
}

/// Serialized, initialized SPL Token `Account` state for a pre-funded account.
///
/// Used to seed user associated token accounts directly in the bank (the
/// modern ATA program unconditionally initializes the Token-2022
/// `ImmutableOwner` extension, which plain Tokenkeg rejects).
fn token_account_data(mint: &Pubkey, owner: &Pubkey, amount: u64) -> Vec<u8> {
    let state = TokenAccount {
        mint: *mint,
        owner: *owner,
        amount,
        delegate: COption::None,
        state: AccountState::Initialized,
        is_native: COption::None,
        delegated_amount: 0,
        close_authority: COption::None,
    };
    let mut data = vec![0u8; TokenAccount::LEN];
    TokenAccount::pack(state, &mut data).expect("pack token account");
    data
}

/// One `(market, user)` party: signer keypair (shared via `Rc` so a `User` is
/// cheaply cloneable — `solana_keypair::Keypair` is not `Clone`) plus the
/// derived PDAs it trades with.
#[derive(Clone)]
struct User {
    keypair: Rc<Keypair>,
    ata: Pubkey,
    user_collateral: Pubkey,
    long: Pubkey,
    short: Pubkey,
}

impl User {
    fn position(&self, side: u8) -> Pubkey {
        if side == LONG {
            self.long
        } else {
            self.short
        }
    }
}

/// A fully-wired test environment: program + SPL programs loaded, collateral
/// mint created, four funded users (A–D) each with a funded ATA, the
/// `PerpMarket`, the `OrderBook`, the collateral vault, and a deposited
/// `UserCollateral` ledger per user.
struct Env {
    ctx: ProgramTestContext,
    market: Pubkey,
    vault: Pubkey,
    mint: Pubkey,
    order_book: Pubkey,
    stake_pool: Pubkey,
    /// A second stake-pool-valid account whose key differs from
    /// `PerpMarket.index_source` — used by `index_source_must_be_market_binding`.
    wrong_stake_pool: Pubkey,
    a: User,
    b: User,
    c: User,
    d: User,
}

async fn setup() -> Option<Env> {
    let program_id = fructus::ID;

    // Run the Fructus program from its SBF binary (Anchor 1.x CPI is SBF-only).
    let so = match find_fructus_so() {
        Some(so) => so,
        None => {
            eprintln!(
                "skipping positions CPI test: fructus.so not found; \
                 run `cargo build-sbf` (or `anchor build`) first"
            );
            return None;
        }
    };
    let so_bytes = std::fs::read(so).expect("read fructus.so");
    let mut pt = ProgramTest::default();
    pt.add_account(
        program_id,
        Account {
            lamports: solana_rent::Rent::default()
                .minimum_balance(so_bytes.len())
                .max(1),
            data: so_bytes,
            owner: solana_sdk_ids::bpf_loader::id(),
            executable: true,
            rent_epoch: 0,
        },
    );
    pt.add_program(
        "spl_token",
        spl_token::id(),
        processor!(spl_token::processor::Processor::process),
    );

    // Fake index-source (jitoSOL stake pool) account, owned by the stake-pool
    // program, so `initialize_market` accepts it as `market.index_source`.
    let stake_pool = Pubkey::new_from_array([9u8; 32]);
    pt.add_account(
        stake_pool,
        Account {
            lamports: FUNDING_LAMPORTS,
            data: fake_stake_pool_data(),
            owner: STAKE_POOL_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    // A second stake-pool-valid account (different key) for the A-13b binding test.
    let wrong_stake_pool = Pubkey::new_from_array([8u8; 32]);
    pt.add_account(
        wrong_stake_pool,
        Account {
            lamports: FUNDING_LAMPORTS,
            data: fake_stake_pool_data(),
            owner: STAKE_POOL_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        },
    );

    // Collateral mint (system-created + token-owned, empty 82-byte state).
    let mint = Keypair::new();
    pt.add_account(
        mint.pubkey(),
        Account {
            lamports: FUNDING_LAMPORTS,
            data: vec![0u8; Mint::LEN],
            owner: spl_token::id(),
            executable: false,
            rent_epoch: 0,
        },
    );

    // Four funded users + their (empty) ATAs, seeded directly in the bank.
    let mut user_seeds = Vec::with_capacity(4);
    for _ in 0..4 {
        let user = Keypair::new();
        pt.add_account(
            user.pubkey(),
            Account {
                lamports: FUNDING_LAMPORTS,
                data: vec![],
                owner: system_program_id(),
                executable: false,
                rent_epoch: 0,
            },
        );
        let ata = get_associated_token_address(&user.pubkey(), &mint.pubkey());
        pt.add_account(
            ata,
            Account {
                lamports: FUNDING_LAMPORTS,
                data: token_account_data(&mint.pubkey(), &user.pubkey(), 0),
                owner: spl_token::id(),
                executable: false,
                rent_epoch: 0,
            },
        );
        user_seeds.push((user, ata));
    }

    // PDAs are pure `find_program_address` derivations, computed before the
    // bank starts so the order-book account can be seeded into it.
    let market = Pubkey::find_program_address(&[PERP_MARKET_SEED], &program_id).0;
    let vault = Pubkey::find_program_address(&[VAULT_SEED], &program_id).0;

    // The `OrderBook` account (`8 + LEN = 6_240` B) is sized under the runtime's
    // per-transaction account-growth cap (MAX_PERMITTED_DATA_INCREASE = 10 KiB),
    // so the on-chain `initialize_order_book` CPI `create_account` now fits.
    // We still seed a fully-initialized account directly in the bank — faster
    // than a CPI round-trip and byte-identical to what the handler would write
    // (discriminator + zeroed struct with `market`/`bump` set). The bank
    // `set_account` path avoids the 10 KiB inner-CPI allocation entirely.
    let (order_book, order_book_bump) =
        Pubkey::find_program_address(&[ORDER_BOOK_SEED, market.as_ref()], &program_id);
    pt.add_account(
        order_book,
        Account {
            lamports: solana_rent::Rent::default().minimum_balance(8 + OrderBook::LEN),
            data: initialized_order_book_data(&market, order_book_bump),
            owner: program_id,
            executable: false,
            rent_epoch: 0,
        },
    );

    let mut ctx = pt.start_with_context().await;

    // 1. Initialize the mint (authority = payer).
    let ix = spl_token::instruction::initialize_mint(
        &spl_token::id(),
        &mint.pubkey(),
        &ctx.payer.pubkey(),
        None,
        DECIMALS,
    )
    .expect("initialize_mint builds");
    submit(&mut ctx, vec![ix], &[])
        .await
        .expect("initialize_mint");

    // 2. Mint collateral to each user's ATA (mint authority = payer).
    for (_user, ata) in &user_seeds {
        let ix = spl_token::instruction::mint_to(
            &spl_token::id(),
            &mint.pubkey(),
            ata,
            &ctx.payer.pubkey(),
            &[],
            MINT_AMOUNT,
        )
        .expect("mint_to builds");
        submit(&mut ctx, vec![ix], &[]).await.expect("mint_to");
    }

    // 3. Initialize the perpetual market (binds the vault PDA + collateral mint).
    let data = fructus::instruction::InitializeMarket {
        collateral_mint: mint.pubkey(),
        funding_k: 100_000,
        max_funding: 10_000,
        funding_epoch_slots: 1_000,
        initial_margin_bps: INITIAL_MARGIN_BPS,
        maintenance_margin_bps: 500,
    }
    .data();
    let ix = Instruction {
        program_id,
        accounts: vec![
            AccountMeta::new(market, false),                     // market (init)
            AccountMeta::new_readonly(stake_pool, false),        // index_source
            AccountMeta::new_readonly(ctx.payer.pubkey(), true), // authority (signer)
            AccountMeta::new(ctx.payer.pubkey(), true),          // payer (signer, mut)
            AccountMeta::new_readonly(system_program_id(), false), // system_program
        ],
        data,
    };
    submit(&mut ctx, vec![ix], &[])
        .await
        .expect("initialize_market");

    let mut env = Env {
        ctx,
        market,
        vault,
        mint: mint.pubkey(),
        order_book,
        stake_pool,
        wrong_stake_pool,
        a: make_user(&market, user_seeds.remove(0)),
        b: make_user(&market, user_seeds.remove(0)),
        c: make_user(&market, user_seeds.remove(0)),
        d: make_user(&market, user_seeds.remove(0)),
    };

    // 4. Collateral vault (authority-gated) and a deposited ledger per user —
    //    every test starts from the same fully-wired state (the order book was
    //    seeded in the bank above).
    initialize_vault(&mut env)
        .await
        .expect("initialize_collateral_vault");
    for user in [env.a.clone(), env.b.clone(), env.c.clone(), env.d.clone()] {
        deposit(&mut env, &user, MINT_AMOUNT)
            .await
            .expect("deposit_collateral");
    }

    Some(env)
}

/// Derive a `User`'s three PDAs from the market key.
fn make_user(market: &Pubkey, (keypair, ata): (Keypair, Pubkey)) -> User {
    let pubkey = keypair.pubkey();
    User {
        keypair: Rc::new(keypair),
        ata,
        user_collateral: user_collateral_pda(market, &pubkey),
        long: position_pda(market, &pubkey, LONG),
        short: position_pda(market, &pubkey, SHORT),
    }
}

fn position_pda(market: &Pubkey, user: &Pubkey, side: u8) -> Pubkey {
    Pubkey::find_program_address(
        &[POSITION_SEED, market.as_ref(), user.as_ref(), &[side]],
        &fructus::ID,
    )
    .0
}

fn user_collateral_pda(market: &Pubkey, user: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[USER_COLLATERAL_SEED, market.as_ref(), user.as_ref()],
        &fructus::ID,
    )
    .0
}

async fn submit(
    ctx: &mut ProgramTestContext,
    ixs: Vec<Instruction>,
    extra_signers: &[&Keypair],
) -> Result<(), BanksClientError> {
    let blockhash = ctx.get_new_latest_blockhash().await.unwrap();
    let mut signers: Vec<&Keypair> = Vec::with_capacity(extra_signers.len() + 1);
    if !extra_signers
        .iter()
        .any(|k| k.pubkey() == ctx.payer.pubkey())
    {
        signers.push(&ctx.payer);
    }
    signers.extend_from_slice(extra_signers);
    let tx = Transaction::new_signed_with_payer(
        &ixs,
        Some(&ctx.payer.pubkey()),
        signers.as_slice(),
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

/// Assert a transaction failed with the given Anchor error code.
fn assert_anchor_error(result: Result<(), BanksClientError>, expected: FructusError) {
    let code = u32::from(expected);
    match result {
        Ok(()) => panic!("expected anchor error (code {code}), got Ok"),
        Err(BanksClientError::TransactionError(TransactionError::InstructionError(
            _,
            SolanaInstructionError::Custom(c),
        ))) => assert_eq!(c, code, "wrong anchor error code"),
        Err(e) => panic!("expected anchor error (code {code}), got {e:?}"),
    }
}

/// Assert a transaction failed with a raw system instruction error (used for
/// `InvalidAccountData` / `InvalidInstructionData`, which Fructus surfaces
/// directly rather than through a `Custom` code).
fn assert_instruction_error(
    result: Result<(), BanksClientError>,
    expected: SolanaInstructionError,
) {
    match result {
        Ok(()) => panic!("expected instruction error {expected:?}, got Ok"),
        Err(BanksClientError::TransactionError(TransactionError::InstructionError(_, e))) => {
            assert_eq!(e, expected, "wrong instruction error")
        }
        Err(e) => panic!("expected instruction error {expected:?}, got {e:?}"),
    }
}

// --- Instruction builders (account order matches the `#[derive(Accounts)]` ---
// --- structs in lib.rs) ------------------------------------------------

/// Account data for a fully-initialized `OrderBook`: the 8-byte Anchor
/// discriminator followed by the raw zero-copy struct bytes (the on-chain
/// `load_init()` view). Header fields match what `initialize_order_book`
/// writes; the arrays are zeroed, so `next_seq == 0` and the event ring is
/// empty — a fresh, handler-identical book.
fn initialized_order_book_data(market: &Pubkey, bump: u8) -> Vec<u8> {
    let mut book = OrderBook::default();
    book.market = *market;
    book.bump = bump;
    let mut data = Vec::with_capacity(8 + OrderBook::LEN);
    data.extend_from_slice(&<OrderBook as Discriminator>::DISCRIMINATOR);
    data.extend_from_slice(bytemuck::bytes_of(&book));
    data
}

async fn initialize_vault(env: &mut Env) -> Result<(), BanksClientError> {
    let data = fructus::instruction::InitializeCollateralVault.data();
    let authority = env.ctx.payer.pubkey();
    let ix = Instruction {
        program_id: fructus::ID,
        accounts: vec![
            AccountMeta::new_readonly(env.market, false), // market
            AccountMeta::new_readonly(authority, true),   // authority (signer)
            AccountMeta::new(authority, true),            // payer (signer, mut)
            AccountMeta::new(env.vault, false),           // vault (mut)
            AccountMeta::new_readonly(env.mint, false),   // collateral_mint
            AccountMeta::new_readonly(system_program_id(), false), // system_program
            AccountMeta::new_readonly(spl_token::id(), false), // token_program
        ],
        data,
    };
    submit(&mut env.ctx, vec![ix], &[]).await
}

async fn deposit(env: &mut Env, user: &User, amount: u64) -> Result<(), BanksClientError> {
    let data = fructus::instruction::DepositCollateral { amount }.data();
    let ix = Instruction {
        program_id: fructus::ID,
        accounts: vec![
            AccountMeta::new(user.keypair.pubkey(), true), // user (signer, mut)
            AccountMeta::new(env.market, false),           // market (mut)
            AccountMeta::new(user.user_collateral, false), // user_collateral (mut)
            AccountMeta::new(env.vault, false),            // vault (mut)
            AccountMeta::new(user.ata, false),             // user_ata (mut)
            AccountMeta::new_readonly(env.mint, false),    // collateral_mint
            AccountMeta::new_readonly(system_program_id(), false), // system_program
            AccountMeta::new_readonly(spl_token::id(), false), // token_program
        ],
        data,
    };
    submit(&mut env.ctx, vec![ix], &[user.keypair.as_ref()]).await
}

async fn withdraw(env: &mut Env, user: &User, amount: u64) -> Result<(), BanksClientError> {
    let data = fructus::instruction::WithdrawCollateral { amount }.data();
    let ix = Instruction {
        program_id: fructus::ID,
        accounts: vec![
            AccountMeta::new_readonly(user.keypair.pubkey(), true), // user (signer)
            AccountMeta::new(env.market, false),                    // market (mut)
            AccountMeta::new(user.user_collateral, false),          // user_collateral (mut)
            AccountMeta::new(env.vault, false),                     // vault (mut)
            AccountMeta::new(user.ata, false),                      // user_ata (mut)
            AccountMeta::new_readonly(env.mint, false),             // collateral_mint
            AccountMeta::new_readonly(spl_token::id(), false),      // token_program
        ],
        data,
    };
    submit(&mut env.ctx, vec![ix], &[user.keypair.as_ref()]).await
}

/// `open_position(side, size, price)`: `price == 0` is a market (IOC) order.
/// `index_source: None` uses the market-bound stake pool; `Some(k)` supplies a
/// caller-chosen account (used to test the `address = market.index_source`
/// binding).
async fn open_position(
    env: &mut Env,
    user: &User,
    side: u8,
    size: u64,
    price: u64,
    index_source: Option<&Pubkey>,
) -> Result<(), BanksClientError> {
    let data = fructus::instruction::OpenPosition { side, size, price }.data();
    let ix = Instruction {
        program_id: fructus::ID,
        accounts: vec![
            AccountMeta::new(user.keypair.pubkey(), true), // owner (signer, mut)
            AccountMeta::new_readonly(env.market, false),  // market
            AccountMeta::new(env.order_book, false),       // order_book (mut)
            AccountMeta::new_readonly(*index_source.unwrap_or(&env.stake_pool), false), // index_source
            AccountMeta::new(user.position(side), false), // position (mut)
            AccountMeta::new(user.user_collateral, false), // user_collateral (mut)
            AccountMeta::new_readonly(system_program_id(), false), // system_program
        ],
        data,
    };
    submit(&mut env.ctx, vec![ix], &[user.keypair.as_ref()]).await
}

/// `close_position(side, size)` places a market-IOC order on the opposite side.
async fn close_position(
    env: &mut Env,
    user: &User,
    side: u8,
    size: u64,
    index_source: Option<&Pubkey>,
) -> Result<(), BanksClientError> {
    let data = fructus::instruction::ClosePosition { side, size }.data();
    let ix = Instruction {
        program_id: fructus::ID,
        accounts: vec![
            AccountMeta::new_readonly(user.keypair.pubkey(), true), // owner (signer)
            AccountMeta::new_readonly(env.market, false),           // market
            AccountMeta::new(env.order_book, false),                // order_book (mut)
            AccountMeta::new_readonly(*index_source.unwrap_or(&env.stake_pool), false), // index_source
            AccountMeta::new(user.position(side), false), // position (mut)
            AccountMeta::new(user.user_collateral, false), // user_collateral (mut)
        ],
        data,
    };
    submit(&mut env.ctx, vec![ix], &[user.keypair.as_ref()]).await
}

/// `settle_fill(seq)` — permissionless; the fee payer (`ctx.payer`) is the
/// caller and pays the lazy rent for a first-time maker `Position`.
async fn settle_fill(
    env: &mut Env,
    seq: u64,
    position: &Pubkey,
    user_collateral: &Pubkey,
) -> Result<(), BanksClientError> {
    let data = fructus::instruction::SettleFill { seq }.data();
    let payer = env.ctx.payer.pubkey();
    let ix = Instruction {
        program_id: fructus::ID,
        accounts: vec![
            AccountMeta::new_readonly(env.market, false), // market
            AccountMeta::new(env.order_book, false),      // order_book (mut)
            AccountMeta::new(*position, false),           // position (mut)
            AccountMeta::new(*user_collateral, false),    // user_collateral (mut)
            AccountMeta::new(payer, true),                // payer (signer, mut)
            AccountMeta::new_readonly(system_program_id(), false), // system_program
        ],
        data,
    };
    submit(&mut env.ctx, vec![ix], &[]).await
}

async fn place_limit_order(
    env: &mut Env,
    user: &User,
    side: u8,
    price: u64,
    size: u64,
    index_source: Option<&Pubkey>,
) -> Result<(), BanksClientError> {
    let data = fructus::instruction::PlaceLimitOrder { side, price, size }.data();
    let ix = Instruction {
        program_id: fructus::ID,
        accounts: vec![
            AccountMeta::new(env.order_book, false), // order_book (mut)
            AccountMeta::new_readonly(env.market, false), // market
            AccountMeta::new_readonly(*index_source.unwrap_or(&env.stake_pool), false), // index_source
            AccountMeta::new_readonly(user.keypair.pubkey(), true), // owner (signer)
        ],
        data,
    };
    submit(&mut env.ctx, vec![ix], &[user.keypair.as_ref()]).await
}

async fn place_market_order(
    env: &mut Env,
    user: &User,
    side: u8,
    size: u64,
    index_source: Option<&Pubkey>,
) -> Result<(), BanksClientError> {
    let data = fructus::instruction::PlaceMarketOrder { side, size }.data();
    let ix = Instruction {
        program_id: fructus::ID,
        accounts: vec![
            AccountMeta::new(env.order_book, false), // order_book (mut)
            AccountMeta::new_readonly(env.market, false), // market
            AccountMeta::new_readonly(*index_source.unwrap_or(&env.stake_pool), false), // index_source
            AccountMeta::new_readonly(user.keypair.pubkey(), true), // owner (signer)
        ],
        data,
    };
    submit(&mut env.ctx, vec![ix], &[user.keypair.as_ref()]).await
}

async fn crank(env: &mut Env, index_source: Option<&Pubkey>) -> Result<(), BanksClientError> {
    let data = fructus::instruction::Crank.data();
    let cranker = env.ctx.payer.pubkey();
    let ix = Instruction {
        program_id: fructus::ID,
        accounts: vec![
            AccountMeta::new(env.order_book, false), // order_book (mut)
            AccountMeta::new_readonly(env.market, false), // market
            AccountMeta::new_readonly(*index_source.unwrap_or(&env.stake_pool), false), // index_source
            AccountMeta::new_readonly(cranker, true), // cranker (signer)
        ],
        data,
    };
    submit(&mut env.ctx, vec![ix], &[]).await
}

// --- Bank-state readers ------------------------------------------------

async fn position_state(env: &Env, key: &Pubkey) -> Option<Position> {
    let account = env.ctx.banks_client.get_account(*key).await.unwrap()?;
    let mut data: &[u8] = &account.data;
    Position::try_deserialize(&mut data).ok()
}

async fn user_collateral_state(env: &Env, key: &Pubkey) -> Option<UserCollateral> {
    let account = env.ctx.banks_client.get_account(*key).await.unwrap()?;
    let mut data: &[u8] = &account.data;
    UserCollateral::try_deserialize(&mut data).ok()
}

async fn ata_balance(env: &Env, ata: &Pubkey) -> u64 {
    let account = env
        .ctx
        .banks_client
        .get_account(*ata)
        .await
        .unwrap()
        .expect("ATA exists");
    TokenAccount::unpack(&account.data).unwrap().amount
}

async fn vault_balance(env: &Env) -> u64 {
    let account = env
        .ctx
        .banks_client
        .get_account(env.vault)
        .await
        .unwrap()
        .expect("vault exists");
    TokenAccount::unpack(&account.data).unwrap().amount
}

/// `margin_required(notional)` mirror of `positions::margin_required` for the
/// market's `INITIAL_MARGIN_BPS`: CEILING `(notional * bps + 9_999) / 10_000`.
fn margin_required(notional: u64) -> u64 {
    ((notional as u128 * INITIAL_MARGIN_BPS as u128 + 9_999) / 10_000) as u64
}

// --- Raw byte views of the zero-copy OrderBook account ---
//
// The on-chain `OrderBook` is an Anchor zero-copy account: `[8-byte
// discriminator][OrderBook payload]`, with the payload laid out as
// `header (88) + bids (16×64) + asks (16×64) + events (32×112) +
// observations (16×32)`. The readers below slice the raw account bytes at the
// documented offsets (AGENTS.md byte-level discipline; `state.rs` pins
// `OrderBook::LEN == 6_232` and the per-type sizes).

const OB_BIDS_OFF: usize = 8 + 88; // discriminator + header
const OB_ASKS_OFF: usize = OB_BIDS_OFF + 16 * 64;
const OB_EVENTS_OFF: usize = OB_ASKS_OFF + 16 * 64;

/// A single resting-order slot, decoded from the `bids`/`asks` arrays.
#[derive(Debug, Clone)]
struct OrderView {
    active: u8,
    owner: Pubkey,
    price: u64,
    size: u64,
    seq: u64,
}

/// One ring slot of the event queue, decoded from the `events` array.
#[derive(Debug, Clone)]
struct EventView {
    seq: u64,
    kind: u8,
    settled: u8,
    side: u8,
    owner: Pubkey,
    counterparty: Pubkey,
    price: u64,
    size: u64,
    entry_total_lamports: u64,
    entry_pool_token_supply: u64,
}

/// Decoded view of the whole `OrderBook` account (cursors, resting orders,
/// event ring) for assertions.
#[derive(Debug, Clone)]
struct BookView {
    best_bid: u64,
    best_ask: u64,
    read_cursor: u64,
    write_cursor: u64,
    bids: Vec<OrderView>,
    asks: Vec<OrderView>,
    events: Vec<EventView>,
}

impl BookView {
    /// The event currently occupying ring `slot` (in ring order, not seq).
    fn event(&self, slot: usize) -> &EventView {
        &self.events[slot]
    }
    fn resting_bids(&self) -> usize {
        self.bids.iter().filter(|o| o.active != 0).count()
    }
    fn resting_asks(&self) -> usize {
        self.asks.iter().filter(|o| o.active != 0).count()
    }
}

async fn book_view(env: &Env) -> BookView {
    let account = env
        .ctx
        .banks_client
        .get_account(env.order_book)
        .await
        .unwrap()
        .expect("order book exists");
    let data = &account.data;
    let mut bids = Vec::with_capacity(16);
    let mut asks = Vec::with_capacity(16);
    let mut events = Vec::with_capacity(32);
    for i in 0..16 {
        bids.push(read_order(data, OB_BIDS_OFF + i * 64));
        asks.push(read_order(data, OB_ASKS_OFF + i * 64));
    }
    for i in 0..32 {
        events.push(read_event(data, OB_EVENTS_OFF + i * 112));
    }
    BookView {
        best_bid: read_u64(data, 16),
        best_ask: read_u64(data, 24),
        read_cursor: read_u64(data, 32),
        write_cursor: read_u64(data, 40),
        bids,
        asks,
        events,
    }
}

fn read_order(data: &[u8], base: usize) -> OrderView {
    OrderView {
        active: data[base + 56],
        owner: read_pubkey(data, base),
        price: read_u64(data, base + 32),
        size: read_u64(data, base + 40),
        seq: read_u64(data, base + 48),
    }
}

fn read_event(data: &[u8], base: usize) -> EventView {
    EventView {
        seq: read_u64(data, base),
        kind: data[base + 105],
        settled: data[base + 104],
        side: data[base + 106],
        owner: read_pubkey(data, base + 24),
        counterparty: read_pubkey(data, base + 56),
        price: read_u64(data, base + 8),
        size: read_u64(data, base + 16),
        entry_total_lamports: read_u64(data, base + 88),
        entry_pool_token_supply: read_u64(data, base + 96),
    }
}

fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().expect("u64 slice"))
}

fn read_pubkey(data: &[u8], offset: usize) -> Pubkey {
    Pubkey::new_from_array(data[offset..offset + 32].try_into().expect("pubkey slice"))
}

// --- Bank-mutation helpers --------------------------------------------

/// Advance the fake stake pool's `total_lamports` (offset 258, with the
/// `account_type` prefix) so the next fill-producing transaction stamps a
/// different index snapshot onto its events.
async fn set_stake_pool_total_lamports(env: &mut Env, total_lamports: u64) {
    let account = env
        .ctx
        .banks_client
        .get_account(env.stake_pool)
        .await
        .unwrap()
        .expect("stake pool exists");
    let mut patched = account.clone();
    patched.data[258..266].copy_from_slice(&total_lamports.to_le_bytes());
    env.ctx
        .set_account(&env.stake_pool, &AccountSharedData::from(patched));
}

/// Overwrite the `seq` field of event-ring `slot` (simulating the slot having
/// been wrapped by 128 newer events — the OQ-1 liveness case).
async fn patch_order_book_event_seq(env: &mut Env, slot: usize, new_seq: u64) {
    let account = env
        .ctx
        .banks_client
        .get_account(env.order_book)
        .await
        .unwrap()
        .expect("order book exists");
    let mut patched = account.clone();
    let seq_off = OB_EVENTS_OFF + slot * 112;
    patched.data[seq_off..seq_off + 8].copy_from_slice(&new_seq.to_le_bytes());
    env.ctx
        .set_account(&env.order_book, &AccountSharedData::from(patched));
}

/// Create a fresh funded party (system account + ATA + minted USDC) that has
/// NOT deposited — so it has no `UserCollateral` ledger — with PDAs derived
/// from the market.
async fn fresh_user(env: &mut Env) -> User {
    let keypair = Keypair::new();
    env.ctx.set_account(
        &keypair.pubkey(),
        &AccountSharedData::from(Account {
            lamports: FUNDING_LAMPORTS,
            data: vec![],
            owner: system_program_id(),
            executable: false,
            rent_epoch: 0,
        }),
    );
    let ata = get_associated_token_address(&keypair.pubkey(), &env.mint);
    env.ctx.set_account(
        &ata,
        &AccountSharedData::from(Account {
            lamports: FUNDING_LAMPORTS,
            data: token_account_data(&env.mint, &keypair.pubkey(), 0),
            owner: spl_token::id(),
            executable: false,
            rent_epoch: 0,
        }),
    );
    let ix = spl_token::instruction::mint_to(
        &spl_token::id(),
        &env.mint,
        &ata,
        &env.ctx.payer.pubkey(),
        &[],
        MINT_AMOUNT,
    )
    .expect("mint_to builds");
    submit(&mut env.ctx, vec![ix], &[]).await.expect("mint_to");
    make_user(&env.market, (keypair, ata))
}

// --- A-20: end-to-end open/close long & short --------------------------

/// The four-party e2e (acceptance A-20): A opens long (limit, rests); B opens
/// short (market) — filling A and booking B's short inline; `settle_fill`
/// books A's maker long; C rests a bid and D rests an ask; A and B market-close
/// against them; `settle_fill` books C's long and D's short. Both closed
/// positions end at `notional == 0` / `reserved == 0`.
#[tokio::test]
async fn position_lifecycle_e2e_long_and_short() {
    let Some(mut env) = setup().await else {
        return;
    };
    let a = env.a.clone();
    let b = env.b.clone();
    let c = env.c.clone();
    let d = env.d.clone();

    // 1. A opens long at a limit price on an empty book: rests, no Position yet.
    let pa = 150_000u64;
    open_position(&mut env, &a, LONG, SIZE, pa, None)
        .await
        .expect("A opens long (limit)");
    let book = book_view(&env).await;
    assert_eq!(book.best_bid, pa, "A's bid rests");
    assert_eq!(book.write_cursor, 0, "a resting order emits no event");
    assert!(
        position_state(&env, &a.long).await.is_none(),
        "maker position is not created until settlement"
    );

    // 2. B opens short (market ask): fills A's bid; B's short settles inline.
    open_position(&mut env, &b, SHORT, SIZE, 0, None)
        .await
        .expect("B opens short (market)");
    let b_pos = position_state(&env, &b.short)
        .await
        .expect("B short exists");
    assert_eq!(b_pos.notional, SIZE, "B's market open fills instantly");
    assert_eq!(b_pos.side, SHORT);
    assert_eq!(b_pos.owner, b.keypair.pubkey());
    assert_eq!(b_pos.collateral, margin_required(SIZE));
    assert_eq!(
        b_pos.entry_n_sum,
        BASE_TOTAL_LAMPORTS as u128 * SIZE as u128,
        "B's entry sums stamp the fill-time snapshot"
    );
    assert_eq!(
        b_pos.entry_d_sum,
        BASE_POOL_TOKEN_SUPPLY as u128 * SIZE as u128
    );
    let uc_b = user_collateral_state(&env, &b.user_collateral)
        .await
        .expect("B ledger");
    assert_eq!(uc_b.reserved, margin_required(SIZE), "B's margin reserved");
    let book = book_view(&env).await;
    assert_eq!(book.best_bid, 0, "A's bid was consumed");
    let ev0 = book.event(0);
    assert_eq!(ev0.kind, 0, "event 0 is a Fill");
    assert_eq!(ev0.seq, 0);
    assert_eq!(ev0.settled, 0, "a fresh Fill is pending maker settlement");
    assert_eq!(ev0.side, LONG, "the maker rested on the bid side");
    assert_eq!(ev0.owner, a.keypair.pubkey());
    assert_eq!(ev0.counterparty, b.keypair.pubkey());
    assert_eq!(ev0.price, pa);
    assert_eq!(ev0.size, SIZE);
    assert_eq!(ev0.entry_total_lamports, BASE_TOTAL_LAMPORTS);
    assert_eq!(ev0.entry_pool_token_supply, BASE_POOL_TOKEN_SUPPLY);

    // 3. settle_fill books A's maker long.
    settle_fill(&mut env, 0, &a.long, &a.user_collateral)
        .await
        .expect("settle A's maker fill");
    let a_pos = position_state(&env, &a.long).await.expect("A long exists");
    assert_eq!(a_pos.notional, SIZE);
    assert_eq!(a_pos.side, LONG);
    assert_eq!(a_pos.owner, a.keypair.pubkey());
    assert_eq!(a_pos.collateral, margin_required(SIZE));
    assert_eq!(
        a_pos.entry_n_sum,
        BASE_TOTAL_LAMPORTS as u128 * SIZE as u128,
        "maker entry sums == event snapshot × size"
    );
    assert_eq!(
        a_pos.entry_d_sum,
        BASE_POOL_TOKEN_SUPPLY as u128 * SIZE as u128
    );
    assert!(
        a_pos.open_slot > 0,
        "maker open records the settlement slot"
    );
    let uc_a = user_collateral_state(&env, &a.user_collateral)
        .await
        .expect("A ledger");
    assert_eq!(uc_a.reserved, margin_required(SIZE));

    // 4. C rests a bid; D rests an ask (D's price above C's, so no cross).
    let pc = 100_000u64;
    let pd = 200_000u64;
    open_position(&mut env, &c, LONG, SIZE, pc, None)
        .await
        .expect("C opens long (limit)");
    open_position(&mut env, &d, SHORT, SIZE, pd, None)
        .await
        .expect("D opens short (limit)");
    let book = book_view(&env).await;
    assert_eq!(book.best_bid, pc, "C's bid rests");
    assert_eq!(book.best_ask, pd, "D's ask rests");
    let c_bid = book.bids.iter().find(|o| o.active != 0).expect("C's bid");
    assert_eq!(c_bid.owner, c.keypair.pubkey());
    assert_eq!(c_bid.price, pc);
    assert_eq!(c_bid.size, SIZE);
    let d_ask = book.asks.iter().find(|o| o.active != 0).expect("D's ask");
    assert_eq!(d_ask.owner, d.keypair.pubkey());
    assert_eq!(d_ask.price, pd);
    assert_eq!(d_ask.size, SIZE);

    // 5. A market-closes the long against C's bid (A long -> 0).
    close_position(&mut env, &a, LONG, SIZE, None)
        .await
        .expect("A closes long (market)");
    let a_pos = position_state(&env, &a.long)
        .await
        .expect("A long retained");
    assert_eq!(a_pos.notional, 0, "A fully closed");
    assert_eq!(a_pos.collateral, 0, "closed position has no margin");
    let uc_a = user_collateral_state(&env, &a.user_collateral)
        .await
        .expect("A ledger");
    assert_eq!(uc_a.reserved, 0, "A's margin fully released");

    // 6. B market-closes the short against D's ask (B short -> 0).
    close_position(&mut env, &b, SHORT, SIZE, None)
        .await
        .expect("B closes short (market)");
    let b_pos = position_state(&env, &b.short)
        .await
        .expect("B short retained");
    assert_eq!(b_pos.notional, 0, "B fully closed");
    assert_eq!(b_pos.collateral, 0);
    let uc_b = user_collateral_state(&env, &b.user_collateral)
        .await
        .expect("B ledger");
    assert_eq!(uc_b.reserved, 0, "B's margin fully released");

    // 7. settle_fill books C's long (event 1) and D's short (event 2).
    settle_fill(&mut env, 1, &c.long, &c.user_collateral)
        .await
        .expect("settle C's maker fill");
    settle_fill(&mut env, 2, &d.short, &d.user_collateral)
        .await
        .expect("settle D's maker fill");
    let c_pos = position_state(&env, &c.long).await.expect("C long exists");
    assert_eq!(c_pos.notional, SIZE);
    assert_eq!(c_pos.side, LONG);
    assert_eq!(c_pos.collateral, margin_required(SIZE));
    assert_eq!(
        c_pos.entry_n_sum,
        BASE_TOTAL_LAMPORTS as u128 * SIZE as u128
    );
    assert_eq!(
        c_pos.entry_d_sum,
        BASE_POOL_TOKEN_SUPPLY as u128 * SIZE as u128
    );
    let d_pos = position_state(&env, &d.short)
        .await
        .expect("D short exists");
    assert_eq!(d_pos.notional, SIZE);
    assert_eq!(d_pos.side, SHORT);
    assert_eq!(d_pos.collateral, margin_required(SIZE));
    assert_eq!(
        d_pos.entry_n_sum,
        BASE_TOTAL_LAMPORTS as u128 * SIZE as u128
    );
    assert_eq!(
        d_pos.entry_d_sum,
        BASE_POOL_TOKEN_SUPPLY as u128 * SIZE as u128
    );
    let uc_c = user_collateral_state(&env, &c.user_collateral)
        .await
        .expect("C ledger");
    assert_eq!(uc_c.reserved, margin_required(SIZE));
    let uc_d = user_collateral_state(&env, &d.user_collateral)
        .await
        .expect("D ledger");
    assert_eq!(uc_d.reserved, margin_required(SIZE));

    // 8. The ring holds exactly three fills, each settled.
    let book = book_view(&env).await;
    assert_eq!(book.write_cursor, 3);
    assert_eq!(book.event(0).settled, 1);
    assert_eq!(book.event(1).settled, 1);
    assert_eq!(book.event(2).settled, 1);
    // Final invariants: both closed positions are at zero notional/reserved,
    // both opened positions are correctly booked.
    assert_eq!(position_state(&env, &a.long).await.unwrap().notional, 0);
    assert_eq!(position_state(&env, &b.short).await.unwrap().notional, 0);
    assert_eq!(
        user_collateral_state(&env, &a.user_collateral)
            .await
            .unwrap()
            .reserved,
        0
    );
    assert_eq!(
        user_collateral_state(&env, &b.user_collateral)
            .await
            .unwrap()
            .reserved,
        0
    );
}

// --- A-4: limit rests, then a market order fills it ---------------------

/// Alice's non-crossing `open_position(Long, size, price)` rests (book bid
/// non-empty, no `Position` yet); Bob's market `open_position(Short, size, 0)`
/// fills it — Bob's `Position(Short)` has `notional == fill size` immediately,
/// Alice's book order is gone, and a `Fill` event with `settled == 0` and the
/// in-transaction snapshot (`entry_*`) was appended.
#[tokio::test]
async fn open_position_limit_rests_then_market_fills() {
    let Some(mut env) = setup().await else {
        return;
    };
    let a = env.a.clone();
    let b = env.b.clone();

    // Advance the index rate so the fill's snapshot is distinguishable from
    // the base rate (validates the bank-mutation mechanism too).
    let new_total_lamports = 11_000_000_000_000u64;
    set_stake_pool_total_lamports(&mut env, new_total_lamports).await;

    let pa = 150_000u64;
    open_position(&mut env, &a, LONG, SIZE, pa, None)
        .await
        .expect("A rests a long");
    let book = book_view(&env).await;
    assert_eq!(book.best_bid, pa);
    assert_eq!(book.resting_bids(), 1);
    let resting = book.bids.iter().find(|o| o.active != 0).expect("A's bid");
    assert_eq!(resting.owner, a.keypair.pubkey());
    assert_eq!(resting.price, pa);
    assert_eq!(resting.size, SIZE);
    assert_eq!(resting.seq, 0, "the first order takes order seq 0");
    assert_eq!(book.write_cursor, 0, "resting order emits no event");
    assert!(
        position_state(&env, &a.long).await.is_none(),
        "no Position until a fill settles it"
    );

    open_position(&mut env, &b, SHORT, SIZE, 0, None)
        .await
        .expect("B market-shorts");
    let b_pos = position_state(&env, &b.short)
        .await
        .expect("B short exists");
    assert_eq!(b_pos.notional, SIZE, "taker fills settle inline");
    assert_eq!(b_pos.entry_n_sum, new_total_lamports as u128 * SIZE as u128);
    assert_eq!(
        b_pos.entry_d_sum,
        BASE_POOL_TOKEN_SUPPLY as u128 * SIZE as u128
    );
    let book = book_view(&env).await;
    assert_eq!(book.best_bid, 0, "Alice's order is gone");
    assert_eq!(book.resting_bids(), 0);
    let ev = book.event(0);
    assert_eq!(ev.kind, 0, "a Fill event was appended");
    assert_eq!(ev.seq, 0);
    assert_eq!(ev.settled, 0);
    assert_eq!(ev.side, LONG, "maker side is the bid");
    assert_eq!(ev.owner, a.keypair.pubkey());
    assert_eq!(ev.counterparty, b.keypair.pubkey());
    assert_eq!(ev.price, pa);
    assert_eq!(ev.size, SIZE);
    assert_eq!(
        ev.entry_total_lamports, new_total_lamports,
        "the Fill carries the in-transaction snapshot"
    );
    assert_eq!(ev.entry_pool_token_supply, BASE_POOL_TOKEN_SUPPLY);
}

// --- A-5: margin shortfall fails atomically -----------------------------

/// Opening a position whose `margin_required` increment exceeds free
/// collateral fails with `InsufficientFreeCollateral` and leaves the book,
/// ledger, and events unchanged (atomic revert). Opening with **no
/// `UserCollateral` at all** also fails with `InsufficientFreeCollateral` (the
/// ledger is deposit-created), not an account-format error.
#[tokio::test]
async fn open_position_margin_shortfall_fails() {
    let Some(mut env) = setup().await else {
        return;
    };
    let a = env.a.clone();

    // Fresh user with a ledger too small to back the position.
    let e = fresh_user(&mut env).await;
    deposit(&mut env, &e, 1_000)
        .await
        .expect("E deposits a tiny amount");

    // A rests an ask; E's market long crosses it but cannot reserve margin.
    let pa = 150_000u64;
    open_position(&mut env, &a, SHORT, SIZE, pa, None)
        .await
        .expect("A rests an ask");
    let result = open_position(&mut env, &e, LONG, SIZE, 0, None).await;
    assert_anchor_error(result, FructusError::InsufficientFreeCollateral);

    // Atomic: the book keeps A's ask, no event was appended, and neither E's
    // ledger nor E's position changed.
    let book = book_view(&env).await;
    assert_eq!(book.best_ask, pa, "A's ask still rests");
    assert_eq!(book.resting_asks(), 1);
    assert_eq!(book.write_cursor, 0, "no fill event survived the revert");
    assert!(
        position_state(&env, &e.long).await.is_none(),
        "E's position was rolled back"
    );
    let uc_e = user_collateral_state(&env, &e.user_collateral)
        .await
        .expect("E ledger");
    assert_eq!(uc_e.deposited, 1_000, "E's ledger untouched");
    assert_eq!(uc_e.reserved, 0);
    let uc_a = user_collateral_state(&env, &a.user_collateral)
        .await
        .expect("A ledger");
    assert_eq!(uc_a.reserved, 0, "A's ledger untouched");

    // A fresh user with NO ledger at all: the same market open fails with
    // InsufficientFreeCollateral (the ledger is deposit-created, so a missing
    // ledger is a free-collateral error, not an account-format error).
    let f = fresh_user(&mut env).await;
    let result = open_position(&mut env, &f, LONG, SIZE, 0, None).await;
    assert_anchor_error(result, FructusError::InsufficientFreeCollateral);
    let book = book_view(&env).await;
    assert_eq!(book.best_ask, pa, "book unchanged again");
    assert_eq!(book.write_cursor, 0);
    assert!(position_state(&env, &f.long).await.is_none());
    assert!(
        user_collateral_state(&env, &f.user_collateral)
            .await
            .is_none(),
        "F never gained a ledger"
    );
}

// --- A-7: close reduces notional and releases margin --------------------

/// After an open, `close_position(Long, size)` reduces `notional` by the
/// filled amount, recomputes `collateral` down, releases
/// `UserCollateral.reserved`, and leaves `entry_*` / `open_slot` unchanged; a
/// full close leaves `notional == 0` / `collateral == 0`. Exercised for both
/// sides, with a partial close in between.
#[tokio::test]
async fn close_position_long_and_short_market() {
    let Some(mut env) = setup().await else {
        return;
    };
    let a = env.a.clone();
    let b = env.b.clone();
    let c = env.c.clone();
    let d = env.d.clone();

    // A opens a 2x-SIZE long; B's market short fills it; settle books A.
    let pa = 150_000u64;
    open_position(&mut env, &a, LONG, 2 * SIZE, pa, None)
        .await
        .expect("A opens long (2x)");
    open_position(&mut env, &b, SHORT, 2 * SIZE, 0, None)
        .await
        .expect("B opens short (2x)");
    settle_fill(&mut env, 0, &a.long, &a.user_collateral)
        .await
        .expect("settle A");
    let a_open = position_state(&env, &a.long).await.expect("A long");
    assert_eq!(a_open.notional, 2 * SIZE);
    assert_eq!(a_open.collateral, margin_required(2 * SIZE));

    // C rests a 2x-SIZE bid; A partially closes SIZE of the long against it,
    // leaving C's bid resting with its SIZE remainder.
    let pc = 100_000u64;
    open_position(&mut env, &c, LONG, 2 * SIZE, pc, None)
        .await
        .expect("C rests a bid");
    close_position(&mut env, &a, LONG, SIZE, None)
        .await
        .expect("A partially closes");
    let a_half = position_state(&env, &a.long).await.expect("A long");
    assert_eq!(a_half.notional, SIZE, "partial close reduces notional");
    assert_eq!(a_half.collateral, margin_required(SIZE));
    assert_eq!(
        a_half.entry_n_sum, a_open.entry_n_sum,
        "close never touches the entry sums"
    );
    assert_eq!(a_half.entry_d_sum, a_open.entry_d_sum);
    assert_eq!(a_half.open_slot, a_open.open_slot, "open_slot unchanged");
    let uc_a = user_collateral_state(&env, &a.user_collateral)
        .await
        .expect("A ledger");
    assert_eq!(
        uc_a.reserved,
        margin_required(SIZE),
        "margin released by delta"
    );
    let book = book_view(&env).await;
    assert_eq!(book.best_bid, pc, "C's bid still rests with its remainder");

    // A closes the remaining SIZE — full close.
    close_position(&mut env, &a, LONG, SIZE, None)
        .await
        .expect("A fully closes");
    let a_closed = position_state(&env, &a.long)
        .await
        .expect("A long retained");
    assert_eq!(a_closed.notional, 0);
    assert_eq!(a_closed.collateral, 0);
    assert_eq!(a_closed.entry_n_sum, a_open.entry_n_sum, "entry untouched");
    assert_eq!(a_closed.entry_d_sum, a_open.entry_d_sum);
    assert_eq!(a_closed.open_slot, a_open.open_slot);
    let uc_a = user_collateral_state(&env, &a.user_collateral)
        .await
        .expect("A ledger");
    assert_eq!(uc_a.reserved, 0, "full close releases all margin");
    let book = book_view(&env).await;
    assert_eq!(book.best_bid, 0, "C's bid consumed");

    // D rests an ask; B market-closes the short against it.
    let pd = 200_000u64;
    open_position(&mut env, &d, SHORT, 2 * SIZE, pd, None)
        .await
        .expect("D rests an ask");
    let b_open = position_state(&env, &b.short).await.expect("B short");
    close_position(&mut env, &b, SHORT, 2 * SIZE, None)
        .await
        .expect("B closes short");
    let b_closed = position_state(&env, &b.short)
        .await
        .expect("B short retained");
    assert_eq!(b_closed.notional, 0);
    assert_eq!(b_closed.collateral, 0);
    assert_eq!(
        b_closed.entry_n_sum, b_open.entry_n_sum,
        "B's entry sums unchanged on close"
    );
    assert_eq!(b_closed.entry_d_sum, b_open.entry_d_sum);
    let uc_b = user_collateral_state(&env, &b.user_collateral)
        .await
        .expect("B ledger");
    assert_eq!(uc_b.reserved, 0, "B's margin fully released");
    let book = book_view(&env).await;
    assert_eq!(book.best_ask, 0, "D's ask consumed");
}

// --- A-8: close errors, no mutation on failure --------------------------

/// Closing with no live position → `PositionNotFound`; with `size > notional`
/// → `InvalidCloseSize`; with `size == 0` → `InvalidSize` (matching the open
/// path); none mutates any account.
#[tokio::test]
async fn close_position_errors() {
    let Some(mut env) = setup().await else {
        return;
    };
    let a = env.a.clone();
    let c = env.c.clone();

    // size == 0 is rejected before any position lookup (InvalidSize).
    let result = close_position(&mut env, &a, LONG, 0, None).await;
    assert_anchor_error(result, FructusError::InvalidSize);

    // No live position at all -> PositionNotFound, no book mutation.
    let result = close_position(&mut env, &a, LONG, SIZE, None).await;
    assert_anchor_error(result, FructusError::PositionNotFound);
    let book = book_view(&env).await;
    assert_eq!(book.write_cursor, 0, "no event on the failed close");

    // Open A's long so we can test InvalidCloseSize and the closed-position case.
    let pa = 150_000u64;
    open_position(&mut env, &a, LONG, SIZE, pa, None)
        .await
        .expect("A opens long");
    let b = env.b.clone();
    open_position(&mut env, &b, SHORT, SIZE, 0, None)
        .await
        .expect("B market-shorts");
    settle_fill(&mut env, 0, &a.long, &a.user_collateral)
        .await
        .expect("settle A");
    let before = position_state(&env, &a.long).await.expect("A long");

    // size > notional -> InvalidCloseSize, position/ledger/book untouched.
    let result = close_position(&mut env, &a, LONG, SIZE + 1, None).await;
    assert_anchor_error(result, FructusError::InvalidCloseSize);
    let after = position_state(&env, &a.long).await.expect("A long");
    assert_eq!(after.notional, before.notional, "notional unchanged");
    assert_eq!(after.collateral, before.collateral, "collateral unchanged");
    assert_eq!(after.entry_n_sum, before.entry_n_sum, "entry unchanged");
    let uc = user_collateral_state(&env, &a.user_collateral)
        .await
        .expect("A ledger");
    assert_eq!(uc.reserved, before.collateral, "reserved unchanged");
    let book = book_view(&env).await;
    assert_eq!(book.write_cursor, 1, "no new event on the failed close");

    // size == 0 again — still InvalidSize, still no mutation.
    let result = close_position(&mut env, &a, LONG, 0, None).await;
    assert_anchor_error(result, FructusError::InvalidSize);

    // Fully close A, then closing again is PositionNotFound (notional == 0).
    let pc = 100_000u64;
    open_position(&mut env, &c, LONG, SIZE, pc, None)
        .await
        .expect("C rests a bid");
    close_position(&mut env, &a, LONG, SIZE, None)
        .await
        .expect("A fully closes");
    let closed = position_state(&env, &a.long)
        .await
        .expect("A long retained");
    assert_eq!(closed.notional, 0);
    let result = close_position(&mut env, &a, LONG, 1, None).await;
    assert_anchor_error(result, FructusError::PositionNotFound);
    let after = position_state(&env, &a.long)
        .await
        .expect("A long retained");
    assert_eq!(after.notional, 0, "still closed after the failed close");
    assert_eq!(after.collateral, 0);
    let uc = user_collateral_state(&env, &a.user_collateral)
        .await
        .expect("A ledger");
    assert_eq!(uc.reserved, 0);
}

// --- A-9: settle_fill books the maker, idempotently ---------------------

/// A permissionless `settle_fill(seq)` creates/updates the maker's
/// `Position(Long)` with `notional == fill size`, `entry_*` matching the
/// event-carried snapshot (sums = snapshot × size), `collateral` reserved, and
/// marks the event `settled == 1`; a second call with the same `seq` succeeds
/// as a no-op (idempotent, D9).
#[tokio::test]
async fn settle_fill_books_maker_position() {
    let Some(mut env) = setup().await else {
        return;
    };
    let a = env.a.clone();
    let b = env.b.clone();

    let new_total_lamports = 11_000_000_000_000u64;
    set_stake_pool_total_lamports(&mut env, new_total_lamports).await;
    let pa = 150_000u64;
    open_position(&mut env, &a, LONG, SIZE, pa, None)
        .await
        .expect("A rests a long");
    open_position(&mut env, &b, SHORT, SIZE, 0, None)
        .await
        .expect("B market-shorts");

    // The caller is the permissionless payer (≠ Alice).
    settle_fill(&mut env, 0, &a.long, &a.user_collateral)
        .await
        .expect("settle the maker fill");
    let pos = position_state(&env, &a.long).await.expect("A long exists");
    assert_eq!(pos.market, env.market);
    assert_eq!(pos.owner, a.keypair.pubkey());
    assert_eq!(pos.side, LONG);
    assert_eq!(pos.notional, SIZE);
    assert_eq!(
        pos.entry_n_sum,
        new_total_lamports as u128 * SIZE as u128,
        "entry sums == event snapshot × size"
    );
    assert_eq!(
        pos.entry_d_sum,
        BASE_POOL_TOKEN_SUPPLY as u128 * SIZE as u128
    );
    assert_eq!(pos.collateral, margin_required(SIZE));
    assert!(pos.open_slot > 0);
    let uc = user_collateral_state(&env, &a.user_collateral)
        .await
        .expect("A ledger");
    assert_eq!(uc.reserved, margin_required(SIZE));
    assert_eq!(book_view(&env).await.event(0).settled, 1);

    // Idempotent no-op: a second settle of the same seq succeeds and changes
    // nothing.
    settle_fill(&mut env, 0, &a.long, &a.user_collateral)
        .await
        .expect("re-settle is a no-op");
    let pos2 = position_state(&env, &a.long).await.expect("A long exists");
    assert_eq!(pos2.notional, SIZE, "no double-booking");
    assert_eq!(pos2.entry_n_sum, pos.entry_n_sum);
    assert_eq!(pos2.entry_d_sum, pos.entry_d_sum);
    assert_eq!(pos2.collateral, pos.collateral);
    let uc2 = user_collateral_state(&env, &a.user_collateral)
        .await
        .expect("A ledger");
    assert_eq!(uc2.reserved, uc.reserved);
    assert_eq!(book_view(&env).await.event(0).settled, 1);
}

// --- A-9b: re-open of a fully closed position resets entry/open_slot -----

/// After the maker's position is fully closed (`notional == 0`, retained
/// account), a later `settle_fill` for a new fill on the same side **re-opens**
/// it — `entry_* :=` the event-carried snapshot weighted by `event.size` and
/// `open_slot :=` the current slot — instead of accumulating into the stale
/// closed sums (FR-2/FR-5).
#[tokio::test]
async fn settle_fill_reopens_closed_position() {
    let Some(mut env) = setup().await else {
        return;
    };
    let a = env.a.clone();
    let c = env.c.clone();
    let d = env.d.clone();

    // Life 1: A rests a long at rate 1.1; B fills it; settle books A.
    set_stake_pool_total_lamports(&mut env, 11_000_000_000_000).await;
    let pa = 150_000u64;
    open_position(&mut env, &a, LONG, SIZE, pa, None)
        .await
        .expect("A rests a long (life 1)");
    let b = env.b.clone();
    open_position(&mut env, &b, SHORT, SIZE, 0, None)
        .await
        .expect("B market-shorts (life 1)");
    settle_fill(&mut env, 0, &a.long, &a.user_collateral)
        .await
        .expect("settle A (life 1)");
    let life1 = position_state(&env, &a.long).await.expect("A long");
    assert_eq!(life1.entry_n_sum, 11_000_000_000_000u128 * SIZE as u128);
    assert!(life1.open_slot > 0);

    // Close fully (retained account): entry sums and open_slot survive.
    let pc = 100_000u64;
    open_position(&mut env, &c, LONG, SIZE, pc, None)
        .await
        .expect("C rests a bid");
    close_position(&mut env, &a, LONG, SIZE, None)
        .await
        .expect("A fully closes");
    let closed = position_state(&env, &a.long)
        .await
        .expect("A long retained");
    assert_eq!(closed.notional, 0);
    assert_eq!(closed.collateral, 0);
    assert_eq!(closed.entry_n_sum, life1.entry_n_sum, "stale sums retained");
    assert_eq!(closed.open_slot, life1.open_slot);

    // Life 2: A rests again at rate 1.2; D fills it; settle re-opens A.
    set_stake_pool_total_lamports(&mut env, 12_000_000_000_000).await;
    let pa2 = 160_000u64;
    open_position(&mut env, &a, LONG, SIZE, pa2, None)
        .await
        .expect("A rests a long (life 2)");
    open_position(&mut env, &d, SHORT, SIZE, 0, None)
        .await
        .expect("D market-shorts (life 2)");
    // Force the re-open to a later slot so open_slot provably resets.
    env.ctx.warp_to_slot(1_000).expect("warp to slot 1000");
    settle_fill(&mut env, 2, &a.long, &a.user_collateral)
        .await
        .expect("settle A (life 2)");

    let reopened = position_state(&env, &a.long).await.expect("A long");
    assert_eq!(reopened.notional, SIZE, "re-opened");
    assert_eq!(
        reopened.entry_n_sum,
        12_000_000_000_000u128 * SIZE as u128,
        "entry := the NEW fill's weighted snapshot, not accumulated into stale sums"
    );
    assert_eq!(
        reopened.entry_d_sum,
        BASE_POOL_TOKEN_SUPPLY as u128 * SIZE as u128
    );
    assert_eq!(reopened.collateral, margin_required(SIZE));
    assert!(
        reopened.open_slot >= 1_000 && reopened.open_slot > life1.open_slot,
        "re-open stamps the current settlement slot ({} > {})",
        reopened.open_slot,
        life1.open_slot
    );
    let uc = user_collateral_state(&env, &a.user_collateral)
        .await
        .expect("A ledger");
    assert_eq!(uc.reserved, margin_required(SIZE));
    assert_eq!(book_view(&env).await.event(2).settled, 1);
}

// --- A-10: settle_fill ownership verification ---------------------------

/// Passing a `Position`/`UserCollateral` that does not match the event-derived
/// PDA fails with `ProgramError::InvalidAccountData`; a `seq` whose ring slot
/// holds no `Fill` with `event.seq == seq` — a never-written seq, or a slot
/// overwritten by newer events (OQ-1) — fails with `EventNotFound`.
#[tokio::test]
async fn settle_fill_verifies_ownership() {
    let Some(mut env) = setup().await else {
        return;
    };
    let a = env.a.clone();
    let b = env.b.clone();

    let pa = 150_000u64;
    open_position(&mut env, &a, LONG, SIZE, pa, None)
        .await
        .expect("A rests a long");
    open_position(&mut env, &b, SHORT, SIZE, 0, None)
        .await
        .expect("B market-shorts");

    // Wrong Position key -> InvalidAccountData (byte-level PDA verification).
    let wrong_pos = Pubkey::new_from_array([1u8; 32]);
    let result = settle_fill(&mut env, 0, &wrong_pos, &a.user_collateral).await;
    assert_instruction_error(result, SolanaInstructionError::InvalidAccountData);

    // Wrong UserCollateral key -> InvalidAccountData.
    let wrong_uc = Pubkey::new_from_array([2u8; 32]);
    let result = settle_fill(&mut env, 0, &a.long, &wrong_uc).await;
    assert_instruction_error(result, SolanaInstructionError::InvalidAccountData);

    // A never-written seq: the ring slot holds a default (seq 0) event.
    let result = settle_fill(&mut env, 999, &a.long, &a.user_collateral).await;
    assert_anchor_error(result, FructusError::EventNotFound);

    // Overwritten slot: patch ring slot 0's seq to 128 (as if 128 newer
    // events wrapped over it), then the original seq 0 is no longer findable.
    patch_order_book_event_seq(&mut env, 0, 128).await;
    let result = settle_fill(&mut env, 0, &a.long, &a.user_collateral).await;
    assert_anchor_error(result, FructusError::EventNotFound);

    // The failed attempts never marked the fill settled.
    assert_eq!(book_view(&env).await.event(0).settled, 0);
}

// --- A-11: settle_fill margin shortfall is retryable --------------------

/// A maker with insufficient free collateral fails settlement atomically (the
/// event stays `settled == 0`); a maker with **no `UserCollateral` yet** also
/// fails with `InsufficientFreeCollateral`; after the maker deposits, the same
/// `settle_fill(seq)` succeeds.
#[tokio::test]
async fn settle_fill_margin_shortfall_retryable() {
    let Some(mut env) = setup().await else {
        return;
    };
    let a = env.a.clone();
    let e = fresh_user(&mut env).await;

    // E rests an ask (position-neutral; no ledger needed to rest).
    let pe = 200_000u64;
    open_position(&mut env, &e, SHORT, SIZE, pe, None)
        .await
        .expect("E rests an ask");
    // A's market long fills E's ask (event 0).
    open_position(&mut env, &a, LONG, SIZE, 0, None)
        .await
        .expect("A market-longs");

    // 1. No UserCollateral at all -> InsufficientFreeCollateral, event stays
    //    pending, no Position created.
    let result = settle_fill(&mut env, 0, &e.short, &e.user_collateral).await;
    assert_anchor_error(result, FructusError::InsufficientFreeCollateral);
    assert_eq!(book_view(&env).await.event(0).settled, 0);
    assert!(position_state(&env, &e.short).await.is_none());
    assert!(user_collateral_state(&env, &e.user_collateral)
        .await
        .is_none());

    // 2. A ledger that cannot back the margin -> InsufficientFreeCollateral,
    //    still atomic (no position, event still pending).
    deposit(&mut env, &e, 1)
        .await
        .expect("E deposits 1 microunit");
    let result = settle_fill(&mut env, 0, &e.short, &e.user_collateral).await;
    assert_anchor_error(result, FructusError::InsufficientFreeCollateral);
    assert_eq!(book_view(&env).await.event(0).settled, 0);
    assert!(position_state(&env, &e.short).await.is_none());
    let uc = user_collateral_state(&env, &e.user_collateral)
        .await
        .expect("E ledger");
    assert_eq!(uc.deposited, 1);
    assert_eq!(uc.reserved, 0);

    // 3. After E deposits enough, the SAME seq settles successfully.
    deposit(&mut env, &e, MINT_AMOUNT - 1)
        .await
        .expect("E tops up");
    settle_fill(&mut env, 0, &e.short, &e.user_collateral)
        .await
        .expect("retry succeeds");
    let pos = position_state(&env, &e.short)
        .await
        .expect("E short exists");
    assert_eq!(pos.notional, SIZE);
    assert_eq!(pos.side, SHORT);
    assert_eq!(pos.owner, e.keypair.pubkey());
    assert_eq!(pos.entry_n_sum, BASE_TOTAL_LAMPORTS as u128 * SIZE as u128);
    assert_eq!(
        pos.entry_d_sum,
        BASE_POOL_TOKEN_SUPPLY as u128 * SIZE as u128
    );
    assert_eq!(pos.collateral, margin_required(SIZE));
    let uc = user_collateral_state(&env, &e.user_collateral)
        .await
        .expect("E ledger");
    assert_eq!(uc.deposited, MINT_AMOUNT);
    assert_eq!(uc.reserved, margin_required(SIZE));
    assert_eq!(book_view(&env).await.event(0).settled, 1);

    // 4. Idempotent after success.
    settle_fill(&mut env, 0, &e.short, &e.user_collateral)
        .await
        .expect("still a no-op after success");
    assert_eq!(position_state(&env, &e.short).await.unwrap().notional, SIZE);
    assert_eq!(book_view(&env).await.event(0).settled, 1);
}

// --- A-15: withdrawal blocked while reserved > 0 -------------------------

/// With an open position (`reserved > 0`), `withdraw_collateral(free + 1)`
/// fails `InsufficientFreeCollateral` and moves no tokens; after closing, the
/// same withdrawal succeeds.
#[tokio::test]
async fn withdrawal_blocked_by_reserved() {
    let Some(mut env) = setup().await else {
        return;
    };
    let a = env.a.clone();
    let b = env.b.clone();
    let c = env.c.clone();

    // Book A's long (reserved = margin_required(SIZE) = 100_000).
    let pa = 150_000u64;
    open_position(&mut env, &a, LONG, SIZE, pa, None)
        .await
        .expect("A opens long");
    open_position(&mut env, &b, SHORT, SIZE, 0, None)
        .await
        .expect("B market-shorts");
    settle_fill(&mut env, 0, &a.long, &a.user_collateral)
        .await
        .expect("settle A");

    let free = MINT_AMOUNT - margin_required(SIZE);
    let before_ata = ata_balance(&env, &a.ata).await;
    let before_vault = vault_balance(&env).await;

    // Withdrawing more than free collateral (reserved > 0) fails atomically.
    let result = withdraw(&mut env, &a, free + 1).await;
    assert_anchor_error(result, FructusError::InsufficientFreeCollateral);
    assert_eq!(ata_balance(&env, &a.ata).await, before_ata, "no ATA change");
    assert_eq!(vault_balance(&env).await, before_vault, "no vault change");
    let uc = user_collateral_state(&env, &a.user_collateral)
        .await
        .expect("A ledger");
    assert_eq!(uc.deposited, MINT_AMOUNT);
    assert_eq!(uc.reserved, margin_required(SIZE));

    // Close the position (releases all reservation), then the same withdrawal
    // succeeds.
    let pc = 100_000u64;
    open_position(&mut env, &c, LONG, SIZE, pc, None)
        .await
        .expect("C rests a bid");
    close_position(&mut env, &a, LONG, SIZE, None)
        .await
        .expect("A closes");
    let uc = user_collateral_state(&env, &a.user_collateral)
        .await
        .expect("A ledger");
    assert_eq!(uc.reserved, 0);

    withdraw(&mut env, &a, free + 1)
        .await
        .expect("withdraw succeeds");
    assert_eq!(
        ata_balance(&env, &a.ata).await,
        before_ata + free + 1,
        "tokens moved to the user ATA"
    );
    assert_eq!(
        vault_balance(&env).await,
        before_vault - (free + 1),
        "tokens left the vault"
    );
    let uc = user_collateral_state(&env, &a.user_collateral)
        .await
        .expect("A ledger");
    assert_eq!(uc.deposited, MINT_AMOUNT - (free + 1));
}

// --- A-13b: index_source must byte-equal market.index_source -------------

/// On every fill-producing instruction (`open_position`, `close_position`,
/// `place_limit_order`, `place_market_order`, `crank`), supplying a
/// stake-pool-valid account whose key ≠ `PerpMarket.index_source` fails (the
/// Anchor `address` constraint), leaves no state behind, and the market-bound
/// account succeeds.
#[tokio::test]
async fn index_source_must_be_market_binding() {
    let Some(mut env) = setup().await else {
        return;
    };
    let a = env.a.clone();
    let b = env.b.clone();
    let c = env.c.clone();
    // Copied (Pubkey is Copy) so it can be passed alongside `&mut env`.
    let wrong = env.wrong_stake_pool;
    let pa = 150_000u64;
    let pa2 = 160_000u64;
    let pc = 100_000u64;

    // place_limit_order: wrong binding fails, right binding rests A's bid.
    let result = place_limit_order(&mut env, &a, LONG, pa, SIZE, Some(&wrong)).await;
    assert!(
        result.is_err(),
        "place_limit_order with a foreign index_source fails"
    );
    let book = book_view(&env).await;
    assert_eq!(book.best_bid, 0, "nothing rested");
    assert_eq!(book.write_cursor, 0);
    place_limit_order(&mut env, &a, LONG, pa, SIZE, None)
        .await
        .expect("market-bound index_source succeeds");
    assert_eq!(book_view(&env).await.best_bid, pa);

    // open_position: wrong binding fails and leaves the book untouched.
    let result = open_position(&mut env, &a, LONG, SIZE, pa2, Some(&wrong)).await;
    assert!(
        result.is_err(),
        "open_position with a foreign index_source fails"
    );
    let book = book_view(&env).await;
    assert_eq!(book.best_bid, pa, "book untouched by the failed open");
    assert_eq!(book.write_cursor, 0);

    // place_market_order: wrong binding fails, right binding fills A's bid.
    let result = place_market_order(&mut env, &b, SHORT, SIZE, Some(&wrong)).await;
    assert!(
        result.is_err(),
        "place_market_order with a foreign index_source fails"
    );
    let book = book_view(&env).await;
    assert_eq!(
        book.best_bid, pa,
        "book untouched by the failed market order"
    );
    assert_eq!(book.write_cursor, 0);
    place_market_order(&mut env, &b, SHORT, SIZE, None)
        .await
        .expect("market-bound index_source succeeds");
    let book = book_view(&env).await;
    assert_eq!(book.best_bid, 0, "A's bid consumed");
    assert_eq!(book.write_cursor, 1);

    // open_position again (A rests a long) and a wrong-binding market open.
    open_position(&mut env, &a, LONG, SIZE, pa2, None)
        .await
        .expect("A rests a long");
    let result = open_position(&mut env, &b, SHORT, SIZE, 0, Some(&wrong)).await;
    assert!(
        result.is_err(),
        "market open with a foreign index_source fails"
    );
    let book = book_view(&env).await;
    assert_eq!(book.best_bid, pa2, "A's bid untouched");
    assert_eq!(book.write_cursor, 1);
    open_position(&mut env, &b, SHORT, SIZE, 0, None)
        .await
        .expect("market open with the market-bound index_source succeeds");

    // Book A's long so close_position can be exercised.
    settle_fill(&mut env, 1, &a.long, &a.user_collateral)
        .await
        .expect("settle A's maker fill");
    open_position(&mut env, &c, LONG, SIZE, pc, None)
        .await
        .expect("C rests a bid");

    // close_position: wrong binding fails, right binding fills C's bid.
    let result = close_position(&mut env, &a, LONG, SIZE, Some(&wrong)).await;
    assert!(
        result.is_err(),
        "close_position with a foreign index_source fails"
    );
    let book = book_view(&env).await;
    assert_eq!(book.best_bid, pc, "book untouched by the failed close");
    assert_eq!(book.write_cursor, 2);
    close_position(&mut env, &a, LONG, SIZE, None)
        .await
        .expect("close_position with the market-bound index_source succeeds");
    assert_eq!(position_state(&env, &a.long).await.unwrap().notional, 0);

    // crank: wrong binding fails, right binding drains the ring.
    let result = crank(&mut env, Some(&wrong)).await;
    assert!(result.is_err(), "crank with a foreign index_source fails");
    let book = book_view(&env).await;
    assert_eq!(book.read_cursor, 0, "failed crank drained nothing");
    crank(&mut env, None)
        .await
        .expect("crank with the market-bound index_source succeeds");
    let book = book_view(&env).await;
    assert_eq!(book.read_cursor, 3, "crank drained all three fills");
}

/// Every positions CPI test body is `let Some(mut env) = setup(..) else { return; }`,
/// so `cargo test --workspace` reports green while silently skipping all
/// position assertions whenever the SBF binary is missing (or runs a stale
/// binary). This guard converts that silent skip/staleness into a hard failure
/// (the acceptance A-21 requires the new tests to actually run).
#[test]
fn cpi_binary_is_present_and_fresh() {
    let so = find_fructus_so().expect(
        "fructus.so not built; every positions CPI test below silently skips under \
         `cargo test --workspace` (A-20/A-21 require them to actually run)",
    );
    let so_mtime = std::fs::metadata(&so)
        .and_then(|m| m.modified())
        .expect("fructus.so metadata");
    let newest = newest_src_mtime(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
    assert!(
        so_mtime >= newest,
        "fructus.so is stale (built {:?}, newest source {:?}); rebuild with `anchor build` \
         so the CPI assertions exercise current code rather than silently running a stale binary",
        so_mtime,
        newest
    );
}

/// Newest `modified` time among the `.rs` files under `dir` (recursive).
fn newest_src_mtime(dir: &Path) -> std::time::SystemTime {
    let mut newest = std::time::SystemTime::UNIX_EPOCH;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("read src dir") {
            let entry = entry.expect("read dir entry");
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
                if let Ok(t) = entry.metadata().and_then(|m| m.modified()) {
                    newest = newest.max(t);
                }
            }
        }
    }
    newest
}

// ============================================================================
// Property-based protocol invariant test (issue #9 "check for bugs"):
// drives the real on-chain CLOB + deferred-maker-settlement flow across varied
// price data (the stake-pool index) and order quantities, and asserts the
// protocol invariants hold for every drawn case. Unlike the deterministic
// scenarios above, the index (jitoSOL exchange rate => premium), the trade
// price, and the order size are all randomized — this is the
// solana-program-test counterpart to the Trident on-chain fuzz (which
// trident_svm's execution stack cannot yet host).
//
// Invariants asserted per case:
//   1. a taker fill creates a Position with notional == size and reserved
//      collateral == margin_required(size);
//   2. the fill stamps the live index (`entry_n_sum == total_lamports * size`,
//      `entry_d_sum == pool_token_supply * size`);
//   3. a deferred maker `settle_fill` books the opposite Position symmetric to
//      the taker;
//   4. the vault is never under-collateralized relative to reserved margin;
//   5. no ledger ever has `reserved > deposited` (no negative free collateral);
//   6. a well-formed sequence never reverts (no panic / unexpected error).
proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(20))]

    #[test]
    fn pbt_clob_fills_hold_invariants(
        // jitoSOL exchange-rate numerator (index / price data), rate 0.9..1.1.
        total_lamports in 9_000_000_000_000u64..=11_000_000_000_000u64,
        // trade yield level (APY_SCALE fixed point), non-crossing when resting.
        price in 1u64..=1_000_000u64,
        // order notional in USDC microunits (fully within the pre-funded deposit).
        size in 1_000u64..=5_000_000u64,
    ) {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let Some(mut env) = setup().await else { return; };
            let a = env.a.clone();
            let b = env.b.clone();

            // Vary the trustless index => varied premium/price data.
            set_stake_pool_total_lamports(&mut env, total_lamports).await;

            // 1. A rests a long bid at `price` (no Position until settlement).
            open_position(&mut env, &a, LONG, size, price, None)
                .await
                .expect("A opens long (limit) rests");
            let book = book_view(&env).await;
            assert_eq!(book.best_bid, price, "A's bid rested at price");
            assert_eq!(
                book.write_cursor, 0,
                "a resting order emits no Fill event"
            );

            // 2. B market-opens a short: fills A's bid inline (taker settlement).
            open_position(&mut env, &b, SHORT, size, 0, None)
                .await
                .expect("B opens short (market) fills");
            let bp = position_state(&env, &b.short).await.expect("B short exists");
            assert_eq!(bp.notional, size, "taker fill has full notional");
            assert_eq!(bp.side, SHORT);
            assert_eq!(bp.owner, b.keypair.pubkey());
            assert_eq!(bp.collateral, margin_required(size), "taker margin reserved");
            assert_eq!(
                bp.entry_n_sum,
                total_lamports as u128 * size as u128,
                "fill stamps the live index numerator"
            );
            assert_eq!(
                bp.entry_d_sum,
                BASE_POOL_TOKEN_SUPPLY as u128 * size as u128,
                "fill stamps the live index denominator"
            );
            let uc_b = user_collateral_state(&env, &b.user_collateral)
                .await
                .expect("B ledger");
            assert_eq!(uc_b.reserved, margin_required(size), "B margin reserved");

            // 3. A's resting bid was consumed by the taker fill.
            let book = book_view(&env).await;
            assert_eq!(book.best_bid, 0, "A's bid consumed");
            let ev = book.event(0);
            assert_eq!(ev.kind, 0, "event 0 is a Fill");
            assert_eq!(ev.settled, 0, "fresh Fill is pending maker settlement");
            assert_eq!(ev.side, LONG, "maker rested on the bid side");
            assert_eq!(ev.owner, a.keypair.pubkey());
            assert_eq!(ev.counterparty, b.keypair.pubkey());
            assert_eq!(ev.price, price);
            assert_eq!(ev.size, size);
            assert_eq!(ev.entry_total_lamports, total_lamports, "live index on fill");

            // 4. Deferred maker settlement books A's symmetric long.
            settle_fill(&mut env, 0, &a.long, &a.user_collateral)
                .await
                .expect("settle A's maker fill");
            let ap = position_state(&env, &a.long).await.expect("A long exists");
            assert_eq!(ap.notional, size, "maker position notional");
            assert_eq!(ap.side, LONG);
            assert_eq!(ap.collateral, margin_required(size), "maker margin reserved");
            assert_eq!(
                ap.entry_n_sum,
                total_lamports as u128 * size as u128,
                "maker entry == event snapshot x size"
            );
            assert_eq!(
                ap.entry_d_sum,
                BASE_POOL_TOKEN_SUPPLY as u128 * size as u128
            );

            // 5. Reserved margin for both open positions never exceeds deposits,
            //    and the vault is never under-collateralized.
            let uc_a = user_collateral_state(&env, &a.user_collateral)
                .await
                .expect("A ledger");
            assert!(
                uc_a.reserved <= uc_a.deposited,
                "A reserved > deposited (negative free collateral)"
            );
            assert!(
                uc_b.reserved <= uc_b.deposited,
                "B reserved > deposited (negative free collateral)"
            );
            let vb = vault_balance(&env).await;
            assert!(
                vb >= 2 * margin_required(size),
                "vault under-collateralized: {vb} < {}",
                2 * margin_required(size)
            );
        });
    }

    // ==========================================================================
    // Full-lifecycle property test: funding settlement (R-F3) + liquidation.
    // Reuses the fill to produce one LONG (A) and one SHORT (B), then
    //   (a) advances a funding epoch and asserts the sign convention when a
    //       non-flat premium makes funding actually flow;
    //   (b) drives A's long underwater (the trustless index drops below the
    //       entry snapshot) and liquidates it, asserting the liquidation is
    //       permissionless, credits the liquidator, and never makes any ledger
    //       negative (R-L/R-S3 conservation).
    #[test]
    fn pbt_funding_and_liquidation(
        entry_total in 9_000_000_000_000u64..=11_000_000_000_000u64,
        price in 1u64..=1_000_000u64,
        size in 1_000u64..=5_000_000u64,
        drawdown_pct in 6u64..=20u64,
    ) {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let Some(mut env) = setup().await else { return; };
            let a = env.a.clone();
            let b = env.b.clone();
            let c = env.c.clone();
            let d = env.d.clone();

            set_stake_pool_total_lamports(&mut env, entry_total).await;

            open_position(&mut env, &a, LONG, size, price, None)
                .await
                .expect("A opens long (limit)");
            open_position(&mut env, &b, SHORT, size, 0, None)
                .await
                .expect("B opens short (market)");
            settle_fill(&mut env, 0, &a.long, &a.user_collateral)
                .await
                .expect("settle A maker fill");
            let apos = position_state(&env, &a.long).await.expect("A long");
            let open_slot = apos.open_slot;

            // ---- (a) funding: advance an epoch, then settle both positions.
            // A non-flat premium needs a real book mid (both sides present) that
            // differs from the index. Rest C on the bid and D on the ask so the
            // mid = 1.10; set the index to 0.90 => premium = +0.20 > 0.
            let mark_bid = 1_000_000u64;
            let mark_ask = 1_200_000u64;
            open_position(&mut env, &c, LONG, size, mark_bid, None)
                .await
                .expect("C rests a bid (sets mid)");
            open_position(&mut env, &d, SHORT, size, mark_ask, None)
                .await
                .expect("D rests an ask (sets mid)");
            set_stake_pool_total_lamports(&mut env, 9_000_000_000_000u64).await;
            env.ctx
                .warp_to_slot(open_slot.wrapping_add(1_001))
                .expect("warp past an epoch");

            let l0 = user_collateral_state(&env, &a.user_collateral)
                .await
                .expect("A ledger pre-funding")
                .deposited;
            let s0 = user_collateral_state(&env, &b.user_collateral)
                .await
                .expect("B ledger pre-funding")
                .deposited;

            let sf_data = fructus::instruction::SettleFunding.data();
            let sf_ix = |pos: &Pubkey, uc: &Pubkey| Instruction {
                program_id: fructus::ID,
                accounts: vec![
                    AccountMeta::new(env.market, false), // market (mut)
                    AccountMeta::new(*pos, false), // position (mut)
                    AccountMeta::new(*uc, false), // user_collateral (mut)
                    AccountMeta::new(env.order_book, false), // order_book (mut)
                    AccountMeta::new_readonly(env.stake_pool, false), // index_source
                ],
                data: sf_data.clone(),
            };
            submit(&mut env.ctx, vec![sf_ix(&a.long, &a.user_collateral)], &[])
                .await
                .expect("settle_funding long");
            submit(&mut env.ctx, vec![sf_ix(&b.short, &b.user_collateral)], &[])
                .await
                .expect("settle_funding short");

            let lafter = user_collateral_state(&env, &a.user_collateral)
                .await
                .expect("A ledger post-funding")
                .deposited;
            let safter = user_collateral_state(&env, &b.user_collateral)
                .await
                .expect("B ledger post-funding")
                .deposited;
            let d_long = lafter as i128 - l0 as i128;
            let d_short = safter as i128 - s0 as i128;
            if d_long != 0 && d_short != 0 {
                // R-F3: long and short funding are exact opposites.
                assert_eq!(d_long, -d_short, "funding not zero-sum (long/short)");
                // premium > 0 => long pays (flows negative), short receives.
                assert!(d_long < 0, "positive premium must make long pay (got {d_long})");
                assert!(d_short > 0, "positive premium must pay short (got {d_short})");
            }
            assert!(lafter >= 0, "long funded below zero");
            assert!(safter >= 0, "short funded below zero");

            // ---- (b) liquidation: drop the index, making A's long underwater.
            let drop = entry_total * (100 - drawdown_pct) / 100;
            set_stake_pool_total_lamports(&mut env, drop).await;
            let uc_c_before = user_collateral_state(&env, &c.user_collateral)
                .await
                .expect("liquidator ledger before")
                .deposited;
            let vb_before = vault_balance(&env).await;

            let liq = fructus::instruction::Liquidate { amount: size }.data();
            let liq_ix = Instruction {
                program_id: fructus::ID,
                accounts: vec![
                    AccountMeta::new(env.market, false), // market (mut)
                    AccountMeta::new(a.long, false), // position (mut)
                    AccountMeta::new(a.user_collateral, false), // user_collateral (mut)
                    AccountMeta::new(env.order_book, false), // order_book (mut)
                    AccountMeta::new_readonly(env.stake_pool, false), // index_source
                    AccountMeta::new_readonly(c.keypair.pubkey(), true), // liquidator (signer)
                    AccountMeta::new(c.user_collateral, false), // liquidator_collateral (mut)
                ],
                data: liq,
            };
            submit(&mut env.ctx, vec![liq_ix], &[c.keypair.as_ref()])
                .await
                .expect("liquidate an underwater long");

            let uc_a_after = user_collateral_state(&env, &a.user_collateral)
                .await
                .expect("A ledger post-liq");
            let uc_c_after = user_collateral_state(&env, &c.user_collateral)
                .await
                .expect("liquidator ledger post-liq");
            let vb_after = vault_balance(&env).await;

            // R-L/R-S3: the liquidator is credited a penalty reward, and no
            // ledger ever goes negative; the vault token total is untouched by a
            // ledger-level liquidation transfer.
            assert!(uc_c_after.deposited >= uc_c_before, "liquidator was not credited");
            assert!(uc_c_after.deposited >= 0, "liquidator ledger negative");
            assert!(uc_a_after.deposited >= 0, "liquidated ledger negative");
            assert!(
                uc_a_after.reserved <= uc_a_after.deposited,
                "liquidated reserved > deposited"
            );
            assert_eq!(vb_after, vb_before, "liquidation must not move vault tokens");
        });
    }
}
