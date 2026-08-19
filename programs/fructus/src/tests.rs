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
fn run_verify(
    ixs: &[BorrowedInstruction],
    publisher: &Pubkey,
    message: &[u8; 32],
) -> Result<()> {
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
