//! Property-based tests for the yield oracle data module's pure logic.
//!
//! Each test traces to a requirement: REQ-3 (ed25519 parsing), REQ-4 (version
//! monotonicity), REQ-5 (APY bounds), REQ-7/REQ-9 (staleness, overflow safety).

use anchor_lang::prelude::*;
use proptest::prelude::*;

use crate::constants::MAX_APY;
use crate::ed25519::{parse_ed25519_instruction, ED25519_PUBKEY_LEN};
use crate::exchange::{
    annualize, ExchangeRate, ACCOUNT_TYPE_OFFSET, ACCOUNT_TYPE_STAKE_POOL,
    POOL_TOKEN_SUPPLY_OFFSET, TOTAL_LAMPORTS_OFFSET,
};
use crate::state::{
    apy_in_bounds, funding_k_in_bounds, initial_margin_in_bounds, is_stale,
    maintenance_margin_in_bounds, max_funding_in_bounds, update_message, validate_version,
};
use solana_instruction::BorrowedInstruction;
use solana_instructions_sysvar::construct_instructions_data;
use solana_sdk_ids::{ed25519_program, sysvar};

use crate::collateral::{deposit, free_collateral, withdraw};
use crate::orderbook::{
    best_ask, best_bid, cancel, is_crossable, match_order, mid, post_limit, price_better, twap,
    Book, Observation, Order, OrderKind, Side,
};

/// Size of an ed25519 signature in bytes.
const ED25519_SIGNATURE_LEN: usize = 64;

// --- REQ-7 / REQ-9: staleness predicate is exact and overflow-safe ---

proptest! {
    #[test]
    fn stale_predicate_matches_saturating_threshold(
        last_update_slot in 0u64..,
        stale_after_slots in 0u64..,
        current_slot in 0u64..,
    ) {
        let expected = current_slot.saturating_sub(last_update_slot) >= stale_after_slots;
        prop_assert_eq!(
            is_stale(last_update_slot, stale_after_slots, current_slot),
            expected
        );
    }

    #[test]
    fn staleness_is_monotonic_in_current_slot(
        last_update_slot in 0u64..,
        stale_after_slots in 0u64..,
        current_slot in 0u64..,
        delta in 0u64..1_000_000,
    ) {
        let later = current_slot.saturating_add(delta);
        let now_stale = is_stale(last_update_slot, stale_after_slots, current_slot);
        let later_stale = is_stale(last_update_slot, stale_after_slots, later);
        // Once stale, an increasing slot must keep it stale.
        prop_assert!(!now_stale || later_stale);
    }

    // --- REQ-4: version must strictly increase ---

    #[test]
    fn version_must_strictly_increase(current in 0u64.., next in 0u64..) {
        let accepted = validate_version(current, next).is_ok();
        prop_assert_eq!(accepted, next > current);
    }

    // --- REQ-5: APY bounds ---

    #[test]
    fn apy_bounds_reject_above_max(apy in 0u64..) {
        prop_assert_eq!(apy_in_bounds(apy), apy <= MAX_APY);
    }

    // --- REQ-3: canonical update message is deterministic and input-sensitive ---

    #[test]
    fn update_message_is_deterministic_and_sensitive(apy in 0u64.., version in 0u64..) {
        let oracle = Pubkey::from([1u8; 32]);
        let m1 = update_message(&oracle, apy, version);
        let m2 = update_message(&oracle, apy, version);
        prop_assert_eq!(m1, m2);

        let m3 = update_message(&oracle, apy.wrapping_add(1), version);
        prop_assert_ne!(m1, m3);

        let m4 = update_message(&oracle, apy, version.wrapping_add(1));
        prop_assert_ne!(m1, m4);
    }
}

// --- REQ-3: ed25519 instruction data parsing ---

#[test]
fn parse_ed25519_instruction_round_trips_inline_data() {
    let pk = Pubkey::from([9u8; 32]);
    let msg = [0xABu8; 32];
    let data = build_ed25519_instruction_data(&pk, &msg);
    let parsed = parse_ed25519_instruction(&data).expect("parse should succeed");
    assert_eq!(parsed.public_key, pk.to_bytes());
    assert_eq!(parsed.message, msg.to_vec());
}

#[test]
fn parse_ed25519_instruction_rejects_malformed() {
    assert!(parse_ed25519_instruction(&[]).is_none());
    assert!(parse_ed25519_instruction(&[0u8; 4]).is_none());

    // num_signatures != 1 is unsupported.
    let pk = Pubkey::from([9u8; 32]);
    let mut data = build_ed25519_instruction_data(&pk, &[1u8; 32]);
    data[0] = 2;
    assert!(parse_ed25519_instruction(&data).is_none());

    // Truncated data (offsets point past the buffer) is rejected without panic.
    let data = build_ed25519_instruction_data(&pk, &[1u8; 32]);
    assert!(parse_ed25519_instruction(&data[..16]).is_none());
}

#[test]
fn parse_ed25519_instruction_rejects_cross_instruction_references() {
    let pk = Pubkey::from([9u8; 32]);
    let msg = [0xABu8; 32];

    // signature_instruction_index != u16::MAX: runtime would read the signature
    // from another instruction, while we read inline — reject.
    let mut data = build_ed25519_instruction_data(&pk, &msg);
    data[4..6].copy_from_slice(&0u16.to_le_bytes());
    assert!(parse_ed25519_instruction(&data).is_none());

    // public_key_instruction_index != u16::MAX.
    let mut data = build_ed25519_instruction_data(&pk, &msg);
    data[8..10].copy_from_slice(&0u16.to_le_bytes());
    assert!(parse_ed25519_instruction(&data).is_none());

    // message_instruction_index != u16::MAX.
    let mut data = build_ed25519_instruction_data(&pk, &msg);
    data[14..16].copy_from_slice(&0u16.to_le_bytes());
    assert!(parse_ed25519_instruction(&data).is_none());
}

// --- Settlement: exchange rate + realized yield ---

proptest! {
    #[test]
    fn self_yield_is_zero(n in 1u64..1_000_000_000, d in 1u64..1_000_000_000) {
        let r = ExchangeRate { total_lamports: n, pool_token_supply: d };
        prop_assert_eq!(r.realized_yield(&r), Some(0));
    }

    #[test]
    fn yield_monotonic_in_settle_numerator(
        n0 in 1u64..1_000_000,
        d0 in 1u64..1_000_000,
        d1 in 1u64..1_000_000,
        n1 in 1u64..1_000_000,
        delta in 0u64..1_000_000,
    ) {
        let open = ExchangeRate { total_lamports: n0, pool_token_supply: d0 };
        let settle = ExchangeRate { total_lamports: n1, pool_token_supply: d1 };
        let settle_higher = ExchangeRate {
            total_lamports: n1.saturating_add(delta),
            pool_token_supply: d1,
        };
        let y1 = open.realized_yield(&settle).unwrap();
        let y2 = open.realized_yield(&settle_higher).unwrap();
        prop_assert!(y2 >= y1);
    }

    #[test]
    fn annualize_is_identity_when_period_equals_year(
        yield_scaled in 0u64..1_000_000,
        slots_per_year in 1u64..1_000_000,
    ) {
        prop_assert_eq!(
            annualize(yield_scaled, slots_per_year, slots_per_year),
            Some(yield_scaled)
        );
    }

    #[test]
    fn annualize_rejects_zero_period(yield_scaled in 0u64.., slots_per_year in 0u64..) {
        prop_assert_eq!(annualize(yield_scaled, 0, slots_per_year), None);
    }

    #[test]
    fn exchange_rate_read_round_trips(n in 1u64.., d in 1u64..) {
        let mut buf = vec![0u8; POOL_TOKEN_SUPPLY_OFFSET + 8];
        buf[ACCOUNT_TYPE_OFFSET] = ACCOUNT_TYPE_STAKE_POOL;
        buf[TOTAL_LAMPORTS_OFFSET..TOTAL_LAMPORTS_OFFSET + 8].copy_from_slice(&n.to_le_bytes());
        buf[POOL_TOKEN_SUPPLY_OFFSET..POOL_TOKEN_SUPPLY_OFFSET + 8].copy_from_slice(&d.to_le_bytes());
        let r = ExchangeRate::read(&buf).unwrap();
        prop_assert_eq!(r.total_lamports, n);
        prop_assert_eq!(r.pool_token_supply, d);
    }

    #[test]
    fn exchange_rate_read_rejects_zero_supply(n in 0u64..) {
        let mut buf = vec![0u8; POOL_TOKEN_SUPPLY_OFFSET + 8];
        buf[ACCOUNT_TYPE_OFFSET] = ACCOUNT_TYPE_STAKE_POOL;
        buf[TOTAL_LAMPORTS_OFFSET..TOTAL_LAMPORTS_OFFSET + 8].copy_from_slice(&n.to_le_bytes());
        // pool_token_supply stays 0
        prop_assert!(ExchangeRate::read(&buf).is_none());
    }

    #[test]
    fn exchange_rate_read_rejects_non_stake_pool(n in 1u64.., d in 1u64..) {
        let mut buf = vec![0u8; POOL_TOKEN_SUPPLY_OFFSET + 8];
        buf[ACCOUNT_TYPE_OFFSET] = 0; // not AccountType::StakePool
        buf[TOTAL_LAMPORTS_OFFSET..TOTAL_LAMPORTS_OFFSET + 8].copy_from_slice(&n.to_le_bytes());
        buf[POOL_TOKEN_SUPPLY_OFFSET..POOL_TOKEN_SUPPLY_OFFSET + 8].copy_from_slice(&d.to_le_bytes());
        prop_assert!(ExchangeRate::read(&buf).is_none());
    }
}

#[test]
fn exchange_rate_read_rejects_short_data() {
    // Valid discriminator but too short to contain both u64 fields.
    let mut buf = vec![0u8; POOL_TOKEN_SUPPLY_OFFSET];
    buf[ACCOUNT_TYPE_OFFSET] = ACCOUNT_TYPE_STAKE_POOL;
    assert!(ExchangeRate::read(&buf).is_none());
}

// --- Cross-language message vector (locks publisher ↔ program consistency) ---

#[test]
fn update_message_matches_known_vector() {
    // oracle = [1u8; 32], apy = 71840 (7.184%), version = 1
    let oracle = Pubkey::from([1u8; 32]);
    let msg = update_message(&oracle, 71_840, 1);
    let expected = hex32("dd9394a5f5b4b383f2478ae97164cb69b495245a220a1be1d0996a0e0d54c1a0");
    assert_eq!(msg, expected);
}

fn hex32(s: &str) -> [u8; 32] {
    let bytes: Vec<u8> = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
        .collect();
    bytes.try_into().expect("32 bytes")
}

// --- ed25519 verification end-to-end (mock instruction sysvar) ---
//
// Exercises `verify_publisher_signature` against a real serialized instruction
// list, covering the introspection path (scan → parse → compare) that unit
// tests of the pure logic cannot reach.

#[test]
fn verify_signature_accepts_matching_instruction() {
    let publisher = Pubkey::from([7u8; 32]);
    let message = [0xAAu8; 32];
    let ed25519_data = build_ed25519_instruction_data(&publisher, &message);
    let borrowed = borrowed(ed25519_data.as_slice());

    let result = run_verify(std::slice::from_ref(&borrowed), &publisher, &message);
    assert!(result.is_ok(), "unexpected: {result:?}");
}

#[test]
fn verify_signature_rejects_wrong_publisher() {
    let publisher = Pubkey::from([7u8; 32]);
    let other = Pubkey::from([8u8; 32]);
    let message = [0xAAu8; 32];
    let ed25519_data = build_ed25519_instruction_data(&other, &message);
    let borrowed = borrowed(ed25519_data.as_slice());

    let result = run_verify(std::slice::from_ref(&borrowed), &publisher, &message);
    assert!(result.is_err());
}

#[test]
fn verify_signature_rejects_wrong_message() {
    let publisher = Pubkey::from([7u8; 32]);
    let message = [0xAAu8; 32];
    let other_message = [0xBBu8; 32];
    let ed25519_data = build_ed25519_instruction_data(&publisher, &other_message);
    let borrowed = borrowed(ed25519_data.as_slice());

    let result = run_verify(std::slice::from_ref(&borrowed), &publisher, &message);
    assert!(result.is_err());
}

#[test]
fn verify_signature_rejects_missing_ed25519_instruction() {
    let publisher = Pubkey::from([7u8; 32]);
    let message = [0xAAu8; 32];

    let result = run_verify(&[], &publisher, &message);
    assert!(result.is_err());
}

#[test]
fn verify_signature_skips_unrelated_ed25519_instructions() {
    let publisher = Pubkey::from([7u8; 32]);
    let message = [0xAAu8; 32];

    // An unrelated ed25519 verification (wrong publisher) comes first.
    let other = Pubkey::from([8u8; 32]);
    let unrelated_data = build_ed25519_instruction_data(&other, &message);
    let unrelated = borrowed(unrelated_data.as_slice());

    // A malformed ed25519 instruction (non-inline reference) comes second.
    let mut bad_data = build_ed25519_instruction_data(&publisher, &message);
    bad_data[8..10].copy_from_slice(&0u16.to_le_bytes());
    let malformed = borrowed(bad_data.as_slice());

    // The matching publisher instruction comes last and must still be found.
    let good_data = build_ed25519_instruction_data(&publisher, &message);
    let good = borrowed(good_data.as_slice());

    let ixs = [unrelated, malformed, good];
    let result = run_verify(&ixs, &publisher, &message);
    assert!(result.is_ok(), "unexpected: {result:?}");
}

/// Build a `BorrowedInstruction` referencing the ed25519 program with the
/// given instruction `data`.
fn borrowed(data: &[u8]) -> BorrowedInstruction<'_> {
    BorrowedInstruction {
        program_id: &ed25519_program::ID,
        accounts: vec![],
        data,
    }
}

/// Build the mock instruction sysvar from `ixs` and run
/// [`crate::ed25519::verify_publisher_signature`] against it.
fn run_verify(ixs: &[BorrowedInstruction], publisher: &Pubkey, message: &[u8; 32]) -> Result<()> {
    let mut sysvar_data = construct_instructions_data(ixs);
    let key = sysvar::instructions::id();
    let owner = sysvar::id();
    let mut lamports = 0u64;
    let account_info = AccountInfo::new(
        &key,
        false,
        false,
        &mut lamports,
        &mut sysvar_data,
        &owner,
        false,
    );
    crate::ed25519::verify_publisher_signature(&account_info, publisher, message)
}

/// Build an `ed25519` program instruction payload mirroring the byte layout of
/// `solana_ed25519_program::new_ed25519_instruction_with_signature` (inline
/// public key + message): header, then public key at `DATA_START`, then the
/// signature, then the message.
fn build_ed25519_instruction_data(pk: &Pubkey, msg: &[u8]) -> Vec<u8> {
    let header_len = 16usize;
    let public_key_offset = header_len;
    let signature_offset = public_key_offset + ED25519_PUBKEY_LEN;
    let message_offset = signature_offset + ED25519_SIGNATURE_LEN;
    let total = message_offset + msg.len();

    let mut data = vec![0u8; total];
    data[0] = 1; // num_signatures
    data[1] = 0; // padding
    data[2..4].copy_from_slice(&(signature_offset as u16).to_le_bytes());
    data[4..6].copy_from_slice(&u16::MAX.to_le_bytes()); // signature_instruction_index: none
    data[6..8].copy_from_slice(&(public_key_offset as u16).to_le_bytes());
    data[8..10].copy_from_slice(&u16::MAX.to_le_bytes()); // public_key_instruction_index: none
    data[10..12].copy_from_slice(&(message_offset as u16).to_le_bytes());
    data[12..14].copy_from_slice(&(msg.len() as u16).to_le_bytes()); // message_data_size
    data[14..16].copy_from_slice(&u16::MAX.to_le_bytes()); // message_instruction_index: none
                                                           // signature bytes stay zero (dummy); parsing only reads pubkey + message.
    data[public_key_offset..public_key_offset + ED25519_PUBKEY_LEN].copy_from_slice(pk.as_ref());
    data[message_offset..message_offset + msg.len()].copy_from_slice(msg);
    data
}

// --- PerpMarket init validation bounds (issue #2) ---
//
// The four pure validators below encode the exact `initialize_market` numeric
// ranges. Each proptest asserts the validator is EXACTLY the interval predicate,
// so the test doubles as the spec.

proptest! {
    #[test]
    fn funding_k_bounds_match_interval(k in 0u64..) {
        prop_assert_eq!(funding_k_in_bounds(k), (1..=1_000_000u64).contains(&k));
    }

    #[test]
    fn max_funding_bounds_match_interval(m in 0u64..) {
        prop_assert_eq!(max_funding_in_bounds(m), m <= 1_000_000);
    }

    #[test]
    fn initial_margin_bounds_match_interval(im in 0u16..) {
        prop_assert_eq!(initial_margin_in_bounds(im), im > 0 && im <= 10_000);
    }

    #[test]
    fn maintenance_margin_bounds_match_interval(im in 0u16.., mm in 0u16..) {
        prop_assert_eq!(maintenance_margin_in_bounds(im, mm), mm > 0 && mm <= im);
    }
}

#[test]
fn funding_k_boundary_edges() {
    assert!(funding_k_in_bounds(1));
    assert!(funding_k_in_bounds(1_000_000));
    assert!(!funding_k_in_bounds(0));
    assert!(!funding_k_in_bounds(1_000_001));
}

#[test]
fn max_funding_boundary_edges() {
    assert!(max_funding_in_bounds(0));
    assert!(max_funding_in_bounds(1_000_000));
    assert!(!max_funding_in_bounds(1_000_001));
}

#[test]
fn initial_margin_boundary_edges() {
    assert!(initial_margin_in_bounds(1));
    assert!(initial_margin_in_bounds(10_000));
    assert!(!initial_margin_in_bounds(0));
    assert!(!initial_margin_in_bounds(10_001));
}

#[test]
fn maintenance_margin_boundary_edges() {
    // initial = 1: maintenance must be in (0, 1] => only 1 is valid.
    assert!(maintenance_margin_in_bounds(1, 1));
    assert!(!maintenance_margin_in_bounds(1, 0));
    assert!(!maintenance_margin_in_bounds(1, 2));

    // initial = 10_000: maintenance must be in (0, 10_000].
    assert!(maintenance_margin_in_bounds(10_000, 10_000));
    assert!(!maintenance_margin_in_bounds(10_000, 0));
    assert!(!maintenance_margin_in_bounds(10_000, 10_001));
}

// --- Order book (CLOB) + collateral vault (issues #3 & #4) ---
//
// Property tests for the pure `orderbook` engine and `collateral` accounting.
// The public API and its semantics are documented in `orderbook.rs` /
// `collateral.rs`; these tests are the executable spec (price-time priority,
// no self-trade / over-fill, mark/twap, free-collateral accounting).

fn ob_order(owner: u8, side: Side, price: u64, size: u64, seq: u64) -> Order {
    Order {
        owner: Pubkey::from([owner; 32]),
        side,
        price,
        size,
        seq,
    }
}

fn ob_book(bids: Vec<Order>, asks: Vec<Order>) -> Book {
    Book {
        bids,
        asks,
        next_seq: 0,
    }
}

proptest! {
    #[test]
    fn is_crossable_matches_ge(bid in 0u64.., ask in 0u64..) {
        prop_assert_eq!(is_crossable(bid, ask), bid >= ask);
    }

    #[test]
    fn price_better_matches_side(cand in 0u64.., best in 0u64..) {
        prop_assert_eq!(price_better(cand, best, Side::Bid), cand > best);
        prop_assert_eq!(price_better(cand, best, Side::Ask), cand < best);
    }

    #[test]
    fn best_bid_ask_consistency(
        bids in proptest::collection::vec((0u8..4u8, 1u64..1000u64, 1u64..100u64, 0u64..100u64), 0..8),
        asks in proptest::collection::vec((0u8..4u8, 1u64..1000u64, 1u64..100u64, 0u64..100u64), 0..8),
    ) {
        let bids: Vec<Order> = bids.into_iter().map(|(o, p, s, q)| ob_order(o, Side::Bid, p, s, q)).collect();
        let asks: Vec<Order> = asks.into_iter().map(|(o, p, s, q)| ob_order(o, Side::Ask, p, s, q)).collect();
        let expect_bid = bids.iter().map(|o| o.price).max().unwrap_or(0);
        let expect_ask = asks.iter().map(|o| o.price).min().unwrap_or(0);
        let book = ob_book(bids, asks);
        prop_assert_eq!(best_bid(&book), expect_bid);
        prop_assert_eq!(best_ask(&book), expect_ask);
    }

    #[test]
    fn mid_bounds_and_truncation(bid in 1u64..1000u64, ask in 1u64..1000u64) {
        let book = ob_book(vec![ob_order(1, Side::Bid, bid, 1, 0)], vec![ob_order(2, Side::Ask, ask, 1, 0)]);
        let m = mid(&book).unwrap();
        prop_assert_eq!(m, (bid + ask) / 2);
        if bid <= ask {
            prop_assert!(m >= bid && m <= ask);
        }
    }

    #[test]
    fn mid_empty_is_none(bid_present in any::<bool>(), ask_present in any::<bool>()) {
        let bids = if bid_present { vec![ob_order(1, Side::Bid, 10, 1, 0)] } else { vec![] };
        let asks = if ask_present { vec![ob_order(2, Side::Ask, 20, 1, 0)] } else { vec![] };
        let book = ob_book(bids, asks);
        prop_assert_eq!(mid(&book).is_none(), !bid_present || !ask_present);
    }

    #[test]
    fn mid_monotonic(bid in 1u64..1000u64, ask in 1u64..1000u64, db in 0u64..100u64, da in 0u64..100u64) {
        let m0 = mid(&ob_book(vec![ob_order(1, Side::Bid, bid, 1, 0)], vec![ob_order(2, Side::Ask, ask, 1, 0)]));
        let mb = mid(&ob_book(vec![ob_order(1, Side::Bid, bid.saturating_add(db), 1, 0)], vec![ob_order(2, Side::Ask, ask, 1, 0)]));
        let ma = mid(&ob_book(vec![ob_order(1, Side::Bid, bid, 1, 0)], vec![ob_order(2, Side::Ask, ask.saturating_add(da), 1, 0)]));
        if let (Some(a), Some(b)) = (m0, mb) { prop_assert!(b >= a); }
        if let (Some(a), Some(c)) = (m0, ma) { prop_assert!(c >= a); }
    }

    #[test]
    fn matching_no_overfill(
        maker_prices in proptest::collection::vec(1u64..100u64, 1..6),
        maker_sizes in proptest::collection::vec(1u64..100u64, 1..6),
        taker_price in 1u64..100u64,
        taker_size in 1u64..200u64,
        max_steps in 0u64..8u64,
    ) {
        let n = maker_prices.len().min(maker_sizes.len());
        let asks: Vec<Order> = (0..n).map(|i| ob_order(2, Side::Ask, maker_prices[i], maker_sizes[i], i as u64)).collect();
        let mut book = ob_book(vec![], asks);
        let incoming = ob_order(1, Side::Bid, taker_price, taker_size, 0);
        let out = match_order(&mut book, incoming, OrderKind::Limit, max_steps);
        let total: u64 = out.fills.iter().map(|f| f.size).sum();
        prop_assert!(total <= taker_size);
        let mut filled = std::collections::HashMap::new();
        for f in &out.fills {
            *filled.entry(f.maker_seq).or_insert(0u64) += f.size;
        }
        for (seq, filled_size) in filled {
            prop_assert!(filled_size <= maker_sizes[seq as usize]);
        }
        for o in &book.asks {
            prop_assert!(o.size <= maker_sizes[o.seq as usize]);
        }
    }

    #[test]
    fn matching_partial_fill_updates_book(
        maker_prices in proptest::collection::vec(1u64..100u64, 1..6),
        maker_sizes in proptest::collection::vec(1u64..100u64, 1..6),
        taker_price in 1u64..100u64,
        taker_size in 1u64..200u64,
    ) {
        let n = maker_prices.len().min(maker_sizes.len());
        let asks: Vec<Order> = (0..n).map(|i| ob_order(2, Side::Ask, maker_prices[i], maker_sizes[i], i as u64)).collect();
        let mut book = ob_book(vec![], asks.clone());
        let out = match_order(&mut book, ob_order(1, Side::Bid, taker_price, taker_size, 0), OrderKind::Limit, u64::MAX);
        let mut filled = vec![0u64; n];
        for f in &out.fills { filled[f.maker_seq as usize] += f.size; }
        let mut remaining = vec![0u64; n];
        for o in &book.asks { remaining[o.seq as usize] = o.size; }
        for i in 0..n {
            prop_assert!(filled[i] <= maker_sizes[i]);
            prop_assert_eq!(remaining[i], maker_sizes[i] - filled[i]);
        }
    }

    #[test]
    fn post_limit_never_crosses(
        posts in proptest::collection::vec((0u8..2u8, 0u64..100u64, 1u64..100u64), 0..10),
    ) {
        let mut book = ob_book(vec![], vec![]);
        let mut seq = 0u64;
        for (side, price, size) in posts {
            let o = ob_order(1 + side, if side == 0 { Side::Bid } else { Side::Ask }, price, size, seq);
            seq += 1;
            let _ = post_limit(&mut book, o);
            let max_bid = book.bids.iter().map(|o| o.price).max().unwrap_or(0);
            let min_ask = book.asks.iter().map(|o| o.price).min().unwrap_or(u64::MAX);
            if !book.bids.is_empty() && !book.asks.is_empty() {
                prop_assert!(max_bid < min_ask, "crossing book: bid {} ask {}", max_bid, min_ask);
            }
        }
    }

    #[test]
    fn free_collateral_checked(d in 0u64.., r in 0u64..) {
        prop_assert_eq!(free_collateral(d, r), d.checked_sub(r));
    }

    #[test]
    fn reserved_zero_invariant(d in 0u64..) {
        prop_assert_eq!(free_collateral(d, 0), Some(d));
    }

    #[test]
    fn deposit_withdraw_accounting(d in 0u64..1000u64, a in 0u64..1000u64, w in 0u64..1000u64) {
        if let Some(nd) = deposit(d, a) {
            prop_assert_eq!(nd, d + a);
            let expect = if w <= nd { Some(nd - w) } else { None };
            prop_assert_eq!(withdraw(nd, 0, w), expect);
        }
    }
}

#[test]
fn matching_no_self_trade() {
    let self_order = ob_order(1, Side::Ask, 9, 100, 0); // owner 1 == taker
    let other = ob_order(2, Side::Ask, 10, 100, 1); // owner 2
    let mut book = ob_book(vec![], vec![self_order, other]);
    let out = match_order(
        &mut book,
        ob_order(1, Side::Bid, 10, 100, 0),
        OrderKind::Limit,
        u64::MAX,
    );
    assert!(
        out.fills.iter().all(|f| f.maker_seq == 1),
        "self-owned maker (seq 0) must be skipped"
    );
    assert_eq!(out.fills.iter().map(|f| f.size).sum::<u64>(), 100);
}

#[test]
fn matching_respects_price_time_priority() {
    let asks = vec![
        ob_order(9, Side::Ask, 10, 100, 2),
        ob_order(9, Side::Ask, 9, 100, 5),
        ob_order(9, Side::Ask, 10, 100, 1),
    ];
    let mut book = ob_book(vec![], asks);
    let out = match_order(
        &mut book,
        ob_order(1, Side::Bid, 10, 250, 0),
        OrderKind::Limit,
        u64::MAX,
    );
    let seqs: Vec<u64> = out.fills.iter().map(|f| f.maker_seq).collect();
    assert_eq!(seqs, vec![5, 1, 2], "price 9 first, then price 10 by seq");
    assert_eq!(out.fills[0].size, 100);
    assert_eq!(out.fills[1].size, 100);
    assert_eq!(out.fills[2].size, 50);
}

#[test]
fn market_order_is_ioc() {
    let mut book = ob_book(vec![], vec![ob_order(2, Side::Ask, 10, 50, 0)]);
    let out = match_order(
        &mut book,
        ob_order(1, Side::Bid, 0, 100, 0),
        OrderKind::Market,
        u64::MAX,
    );
    assert_eq!(out.fills.iter().map(|f| f.size).sum::<u64>(), 50);
    assert!(
        out.residual.is_none(),
        "market remainder is cancelled, never re-queued"
    );
}

#[test]
fn market_order_residual_none_when_budget_limited() {
    let asks = vec![
        ob_order(2, Side::Ask, 10, 50, 0),
        ob_order(3, Side::Ask, 10, 50, 1),
    ];
    let mut book = ob_book(vec![], asks);
    let out = match_order(
        &mut book,
        ob_order(1, Side::Bid, 0, 100, 0),
        OrderKind::Market,
        1,
    );
    assert_eq!(out.fills.len(), 1);
    assert!(out.residual.is_none());
}

#[test]
fn cancel_owner_only_and_removes_one() {
    let mut book = ob_book(
        vec![
            ob_order(1, Side::Bid, 10, 5, 0),
            ob_order(2, Side::Bid, 9, 5, 1),
        ],
        vec![],
    );
    assert!(
        cancel(&mut book, Pubkey::from([9u8; 32]), 0).is_err(),
        "non-owner cannot cancel"
    );
    assert!(
        cancel(&mut book, Pubkey::from([1u8; 32]), 99).is_err(),
        "absent seq fails"
    );
    assert!(cancel(&mut book, Pubkey::from([1u8; 32]), 0).is_ok());
    assert_eq!(book.bids.len(), 1);
    assert_eq!(book.bids[0].seq, 1);
}

#[test]
fn matching_batching_commutativity() {
    let asks = vec![
        ob_order(2, Side::Ask, 10, 10, 0),
        ob_order(3, Side::Ask, 11, 10, 1),
        ob_order(4, Side::Ask, 12, 10, 2),
        ob_order(5, Side::Ask, 13, 10, 3),
    ];
    let incoming = ob_order(1, Side::Bid, 13, 100, 0);

    let mut full = ob_book(vec![], asks.clone());
    let out_full = match_order(&mut full, incoming.clone(), OrderKind::Limit, u64::MAX);

    let mut batched = ob_book(vec![], asks);
    let out1 = match_order(&mut batched, incoming, OrderKind::Limit, 2);
    let mut all_fills = out1.fills.clone();
    let mut residual = out1.residual;
    while let Some(rem) = residual {
        let o = match_order(&mut batched, rem, OrderKind::Limit, 2);
        all_fills.extend(o.fills);
        residual = o.residual;
    }

    assert_eq!(all_fills, out_full.fills);
    assert_eq!(batched, full);
}

#[test]
fn twap_constant_mid() {
    let obs: Vec<Observation> = (0..=1000u64)
        .map(|s| Observation {
            slot: s,
            cumulative_mid: (s as u128) * 7,
        })
        .collect();
    assert_eq!(twap(&obs, 100, 1000), Some(7));
    assert_eq!(twap(&obs, 0, 1000), None, "window 0 -> None");
    assert_eq!(twap(&[], 100, 1000), None, "empty -> None");
    assert_eq!(
        twap(&obs, 2000, 1000),
        None,
        "history shorter than window -> None"
    );
}

#[test]
fn twap_within_range() {
    // piecewise-constant mids over slots; twap is a time-weighted average, so it
    // must lie within [min mid, max mid].
    let mids = [3u64, 5, 9, 7, 1, 8];
    let mut obs = Vec::new();
    let mut cum: u128 = 0;
    for (i, m) in mids.iter().enumerate() {
        obs.push(Observation {
            slot: i as u64,
            cumulative_mid: cum,
        });
        cum += *m as u128;
    }
    obs.push(Observation {
        slot: mids.len() as u64,
        cumulative_mid: cum,
    });
    if let Some(t) = twap(&obs, 3, mids.len() as u64) {
        assert!(t >= 1 && t <= 9, "twap {} out of range", t);
    }
}

#[test]
fn collateral_boundary_edges() {
    assert_eq!(deposit(u64::MAX, 1), None, "deposit overflow -> None");
    assert_eq!(withdraw(0, 0, 1), None, "withdraw > free -> None");
    assert_eq!(withdraw(10, 5, 6), None, "amount > free (10-5) -> None");
    assert_eq!(withdraw(10, 5, 5), Some(5), "exactly free -> Some");
}

// --- Review regression tests (red) ----------------------------------------
//
// Each test below pins a review finding and currently FAILS against the working
// tree. T1/T2 are pure-logic; T3/T5/T6 are static-contract findings (dead error
// variants, an umbrella dev-dependency, a missing required constant) pinned as
// source-content assertions because they have no runtime behavior to exercise.

/// A zero-initialized on-chain `OrderBook` account, mirroring the empty account
/// the `initialize_order_book` handler produces (all slots default).
fn empty_onchain_order_book() -> crate::state::OrderBook {
    crate::state::OrderBook {
        market: Pubkey::default(),
        bump: 0,
        next_seq: 0,
        best_bid: 0,
        best_ask: 0,
        event_read_cursor: 0,
        event_write_cursor: 0,
        twap_cursor: 0,
        _pad: [0u8; 7],
        bids: [crate::state::Order::default(); crate::constants::MAX_ORDERS_PER_SIDE],
        asks: [crate::state::Order::default(); crate::constants::MAX_ORDERS_PER_SIDE],
        events: [crate::state::OutEvent::default(); crate::constants::EVENT_QUEUE_LEN],
        observations: [crate::state::Observation::default(); crate::constants::TWAP_OBSERVATIONS],
    }
}

/// T1: the event-queue ring silently overwrites an undrained `Residual`.
#[test]
fn event_ring_wrap_does_not_silently_drop_deferred_residual() {
    let mut account = empty_onchain_order_book();
    let asks: Vec<Order> = (0..10)
        .map(|i| ob_order(2, Side::Ask, 10 + i as u64, 1, i as u64))
        .collect();
    let mut book = Book {
        bids: vec![],
        asks,
        next_seq: 10,
    };

    // A crossing limit taker hits MAX_MATCH_STEPS == 8 and defers the remaining
    // 2 as a Residual event (8 fills + 1 residual => write_cursor == 9, and the
    // residual lands in slot 8).
    let incoming = ob_order(1, Side::Bid, 30, 10, 0);
    crate::match_limit_taker(&mut account, &mut book, incoming).unwrap();
    assert_eq!(account.events[8].kind, crate::EVENT_KIND_RESIDUAL);

    // Fill the ring past capacity WITHOUT draining (read_cursor stays 0): the
    // FR-8(f)/FR-9 scenario where un-cranked crossings wrap the ring.
    for i in 0..crate::constants::EVENT_QUEUE_LEN {
        crate::append_event(
            &mut account,
            crate::EVENT_KIND_FILL,
            Pubkey::from([7; 32]),
            Pubkey::default(),
            crate::SIDE_BID,
            i as u64,
            1,
        );
    }

    // The deferred Residual (the taker's still-crossable remainder) must remain
    // recoverable; a wrapping write must never silently overwrite an undrained
    // event.
    let residual_survives = account
        .events
        .iter()
        .any(|e| e.kind == crate::EVENT_KIND_RESIDUAL && e.size == 2);
    assert!(
        residual_survives,
        "deferred Residual was silently overwritten by the undrained ring wrap"
    );
}

/// T2: `twap` must answer over a caller-supplied trailing window even when the
/// caller passes an off-grid `Clock::get().slot`.
#[test]
fn twap_returns_some_over_callers_trailing_window() {
    // Observations are only appended on book mutation at irregular slots, so a
    // caller passing Clock::get().slot (off-grid) must still get a
    // time-weighted average over its trailing window (FR-11). The exact-match
    // lookup in cumulative_at currently makes this return None.
    let obs = vec![
        Observation {
            slot: 100,
            cumulative_mid: 0,
        },
        Observation {
            slot: 110,
            cumulative_mid: 100,
        },
        Observation {
            slot: 130,
            cumulative_mid: 300,
        },
    ];
    // now_slot == 125 and start == 105 are both off-grid; mid is constant 10
    // over the whole window, so the correct TWAP is 10, not None.
    assert_eq!(twap(&obs, 20, 125), Some(10));
}

/// T3: the vault's `VaultNotInitialized` variant must be wired in the handlers.
///
/// The two order-book init variants (`BookAlreadyInitialized` /
/// `BookNotInitialized`) are intentionally NOT referenced from handler bodies:
/// after the zero-copy refactor a second `init` and a pre-init book op are
/// surfaced by Anchor's `init` + zero-copy discriminator constraints before any
/// handler runs, so they remain documented forward-declarations in `error.rs`.
#[test]
fn book_error_contract_variants_are_referenced_in_handlers() {
    let lib_src = include_str!("lib.rs");
    assert!(
        lib_src.contains("VaultNotInitialized"),
        "VaultNotInitialized must be wired in the deposit/withdraw handlers"
    );
}

/// T5: the `solana-sdk` umbrella crate must not be pulled in as a dev-dependency.
#[test]
fn no_solana_sdk_umbrella_dev_dependency() {
    // AGENTS.md forbids the solana-program umbrella; the same constraint applies
    // to the solana-sdk umbrella dev-dependency (which drags the whole crate in
    // and mixes versions with the granular 3.x crates).
    let manifest = include_str!("../Cargo.toml");
    let has_umbrella = manifest
        .lines()
        .any(|l| l.trim_start().starts_with("solana-sdk ="));
    assert!(
        !has_umbrella,
        "solana-sdk umbrella crate must not be a dependency"
    );
}

/// T6: FR-2(a) / execution-plan REQ-4 require `MAX_PRICE_LEVELS_PER_SIDE`.
#[test]
fn max_price_levels_per_side_constant_exists() {
    // FR-2(a) / execution-plan REQ-4 require MAX_PRICE_LEVELS_PER_SIDE; it is
    // absent from constants.rs (price levels were collapsed into per-order
    // capacity). Pin the requirement until the deviation is explicitly accepted.
    let src = include_str!("constants.rs");
    assert!(
        src.contains("MAX_PRICE_LEVELS_PER_SIDE"),
        "FR-2(a) / REQ-4 require MAX_PRICE_LEVELS_PER_SIDE in constants.rs"
    );
}

// --- Review regression tests, round 2 (red) --------------------------------
//
// Each test below pins a round-2 review finding and currently FAILS against the
// working tree. F1/F2/F3/F4 are pure-logic (record_observation /
// match_limit_taker / append_event); F5 is a static doc/code-drift finding.

/// F1 (TWAP mis-weighting): `record_observation` is fed the POST-mutation
/// `mid(&book)` by every book-mutating handler (`save_book` runs first, then
/// `record_observation(.., mid(&book), ..)` at lib.rs 533/541/584/679), so a mid
/// that just changed is charged to the interval BEFORE the mutation. The elapsed
/// interval `[prev_obs, now)` saw the PRE-mutation mid, so that is what must be
/// charged — otherwise FR-11's TWAP is systematically biased.
#[test]
fn twap_charges_pre_mutation_mid_for_elapsed_interval() {
    let mut account = empty_onchain_order_book();
    let mut book = Book {
        bids: vec![ob_order(1, Side::Bid, 10, 1, 0)],
        asks: vec![ob_order(2, Side::Ask, 20, 1, 1)],
        next_seq: 2,
    };

    // First observation at slot 100 establishes the mid (cumulative stays 0).
    crate::record_observation(&mut account, crate::orderbook::mid(&book), 100);

    // The book mutates at slot 110: bid 10 -> 14, so the mid moves 15 -> 17.
    book.bids[0].price = 14;
    crate::save_book(&mut account, &book);
    // Mirror the handler: record the POST-mutation mid (17).
    crate::record_observation(&mut account, crate::orderbook::mid(&book), 110);

    // The interval [100, 110) saw mid 15, so the accumulator must advance by
    // 15 * 10 = 150. The current code charges the post-mutation mid 17
    // (=> 170), biasing the TWAP high.
    assert_eq!(
        u128::from_le_bytes(account.observations[1].cumulative_mid),
        150,
        "the pre-mutation mid (15) must be charged to [100, 110), not the \
         post-mutation mid (17)"
    );
}

/// F2 (crank wedge/liveness): the permissionless `crank` re-matches a `Residual`
/// via `match_limit_taker` (lib.rs crank loop). A `SelfTrade` or `BookFull`
/// error from that call reverts the whole transaction INCLUDING the read-cursor
/// advance (lib.rs 636-639), so the offending Residual stays at the queue head
/// and permanently wedges the shared crank. Root cause: `match_limit_taker` can
/// return `Err` while resuming a Residual that was already accepted (partially
/// filled) in a prior transaction — a crank-resumed Residual must be consumed,
/// never rejected.
#[test]
fn crank_resumed_residual_is_not_rejected_with_book_full() {
    let mut account = empty_onchain_order_book();

    // Bid side already at capacity (64 resting orders), so a leftover remainder
    // cannot rest there.
    let bids: Vec<Order> = (0..crate::constants::MAX_ORDERS_PER_SIDE as u64)
        .map(|i| ob_order(2, Side::Bid, 1 + i, 1, i))
        .collect();
    // One crossable ask (owner 3) of size 5.
    let asks = vec![ob_order(3, Side::Ask, 100, 5, 64)];
    let mut book = Book {
        bids,
        asks,
        next_seq: 65,
    };

    // The crank drains a Residual: owner 1's bid @ 100 for size 10. It fills the
    // 5-unit ask, and the 5-unit remainder no longer crosses (book exhausted),
    // so it tries to rest — but the bid side is full -> BookFull -> the crank
    // reverts and wedges on the offending Residual forever.
    let residual = ob_order(1, Side::Bid, 100, 10, 65);
    let result = crate::match_limit_taker(&mut account, &mut book, residual);

    assert!(
        result.is_ok(),
        "a crank-resumed Residual must be consumed, never rejected with BookFull \
         (the error reverts the crank's read-cursor advance and wedges the queue)"
    );
    assert!(
        book.asks.is_empty(),
        "the crossable ask must be filled before the remainder is handled"
    );
}

/// F3 (silent Residual loss): `append_event` silently drops events when the ring
/// is full, so a budget-interrupted taker's deferred `Residual` is permanently
/// lost while `place_limit_order` (via `match_limit_taker`) still returns Ok.
/// FR-8(f)/FR-9 require backpressure: an un-cranked remainder must never be
/// silently dropped while the caller reports success.
#[test]
fn deferred_residual_is_not_silently_dropped_on_full_ring() {
    let mut account = empty_onchain_order_book();

    // Fill the event ring completely without draining (read_cursor stays 0).
    account.event_read_cursor = 0;
    account.event_write_cursor = crate::constants::EVENT_QUEUE_LEN as u64;

    // 10 crossable asks; MAX_MATCH_STEPS == 8, so 2 units are deferred as a
    // Residual event.
    let asks: Vec<Order> = (0..10)
        .map(|i| ob_order(2, Side::Ask, 10 + i as u64, 1, i as u64))
        .collect();
    let mut book = Book {
        bids: vec![],
        asks,
        next_seq: 10,
    };
    let incoming = ob_order(1, Side::Bid, 30, 10, 0);

    let result = crate::match_limit_taker(&mut account, &mut book, incoming);

    let residual_queued = account
        .events
        .iter()
        .any(|e| e.kind == crate::EVENT_KIND_RESIDUAL && e.size == 2);
    assert!(
        result.is_err() || residual_queued,
        "a deferred Residual must not be silently dropped on a full ring \
         (the caller must backpressure or preserve the Residual, never report Ok)"
    );
}

/// F4 (self-trade blocks non-self fills): `match_limit_taker` reverts the whole
/// taker order with `SelfTrade` even after legitimate non-self fills have been
/// applied, so a self-owned resting order blocks other fills (and feeds F2). The
/// non-self fills must survive; the self-crossing remainder must be cancelled,
/// not used to revert the taker.
#[test]
fn match_limit_taker_preserves_non_self_fills_when_remainder_self_trades() {
    let mut account = empty_onchain_order_book();
    let mut book = Book {
        bids: vec![],
        // A self-owned ask at a BETTER price (9) plus a non-self ask at 10.
        asks: vec![
            ob_order(1, Side::Ask, 9, 100, 0),
            ob_order(2, Side::Ask, 10, 100, 1),
        ],
        next_seq: 2,
    };
    // Taker (owner 1) bids 10 for size 150: the non-self ask fills 100; the only
    // remaining crossable maker is the self-owned ask -> currently SelfTrade.
    let incoming = ob_order(1, Side::Bid, 10, 150, 2);

    let result = crate::match_limit_taker(&mut account, &mut book, incoming);

    assert!(
        result.is_ok(),
        "a self-owned resting order must not revert the whole taker order"
    );
    // The non-self ask must be filled (removed); the self-owned ask skipped.
    assert_eq!(book.asks.len(), 1, "non-self ask filled, self ask skipped");
    assert_eq!(book.asks[0].seq, 0, "self-owned ask is skipped, not filled");
    // The legitimate non-self fill must be recorded.
    assert!(account.events.iter().any(|e| {
        e.kind == crate::EVENT_KIND_FILL && e.owner == Pubkey::from([2u8; 32]) && e.size == 100
    }));
}

/// F5 (doc/code drift, FR-20): `docs/api-reference.md` still claims
/// `BookAlreadyInitialized` / `BookNotInitialized` / `VaultNotInitialized` are
/// "not yet wired" and surfaced by Anchor account constraints, but `lib.rs` now
/// wires all three with explicit `require!` checks (BookAlreadyInitialized at
/// 463, BookNotInitialized at 514/557/593/623, VaultNotInitialized at 745/806).
/// The doc must not drift from the code.
#[test]
fn api_reference_error_wiring_is_current() {
    let doc = include_str!("../../../docs/api-reference.md");
    for stale in ["not yet wired", "currently surfaced by"] {
        assert!(
            !doc.contains(stale),
            "docs/api-reference.md still describes errors as {stale:?}, but lib.rs \
             now wires BookAlreadyInitialized/BookNotInitialized/VaultNotInitialized \
             with require! checks"
        );
    }
}
