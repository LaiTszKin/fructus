//! Ed25519 signature verification via instruction introspection.
//!
//! The runtime verifies the ed25519 program's instruction *itself* (a native
//! program that fails the transaction on an invalid signature). Our job is to
//! bind that already-verified signature to the right publisher and message:
//! find the `ed25519` instruction in the current transaction, parse it, and
//! confirm its public key and message match what we expect.
//!
//! All comparisons are done on raw bytes (`as_ref` / `to_bytes`) to stay
//! agnostic to the `Pubkey` vs `Address` type version in the dependency graph.

use anchor_lang::prelude::*;
use solana_instructions_sysvar::load_instruction_at_checked;
use solana_sdk_ids::ed25519_program;

use crate::error::FructusError;

/// Size of an ed25519 public key in bytes.
pub const ED25519_PUBKEY_LEN: usize = 32;

/// A parsed ed25519 verify instruction (inline public key + message).
#[derive(Debug, PartialEq)]
pub struct Ed25519Verification {
    pub public_key: [u8; 32],
    pub message: Vec<u8>,
}

/// Parse the data of an `ed25519` program instruction.
///
/// Layout (see `solana_program::ed25519_program::Instruction` /
/// `solana_ed25519_program::Ed25519SignatureOffsets`):
/// ```text
/// 0        num_signatures: u8
/// 1        padding: u8
/// 2..4     signature_offset: u16 (LE)
/// 4..6     signature_instruction_index: u16 (LE)
/// 6..8     public_key_offset: u16 (LE)
/// 8..10    public_key_instruction_index: u16 (LE)
/// 10..12   message_offset: u16 (LE)
/// 12..14   message_data_size: u16 (LE)
/// 14..16   message_instruction_index: u16 (LE)
/// ...      inline signature / public key / message at the declared offsets
/// ```
///
/// The runtime resolves the signature, public key and message from the
/// instruction named by each `*_instruction_index` (`u16::MAX` means "inline",
/// anything else points at the data of another instruction in the transaction).
/// We only ever read the inline copies, so to guarantee we compare exactly the
/// bytes the runtime verified we reject any instruction that references a
/// different instruction (`*_instruction_index != u16::MAX`).
///
/// Returns `None` for anything malformed, out of bounds, or non-inline.
pub fn parse_ed25519_instruction(data: &[u8]) -> Option<Ed25519Verification> {
    if data.len() < 16 {
        return None;
    }
    // Only single-signature updates are supported.
    if data[0] != 1 {
        return None;
    }
    let signature_instruction_index = u16::from_le_bytes([data[4], data[5]]);
    let public_key_instruction_index = u16::from_le_bytes([data[8], data[9]]);
    let message_instruction_index = u16::from_le_bytes([data[14], data[15]]);
    if signature_instruction_index != u16::MAX
        || public_key_instruction_index != u16::MAX
        || message_instruction_index != u16::MAX
    {
        return None;
    }

    let public_key_offset = u16::from_le_bytes([data[6], data[7]]) as usize;
    let message_offset = u16::from_le_bytes([data[10], data[11]]) as usize;
    let message_data_size = u16::from_le_bytes([data[12], data[13]]) as usize;

    let pk_end = public_key_offset.checked_add(ED25519_PUBKEY_LEN)?;
    let msg_end = message_offset.checked_add(message_data_size)?;

    let pk_bytes = data.get(public_key_offset..pk_end)?;
    let mut public_key = [0u8; 32];
    public_key.copy_from_slice(pk_bytes);
    let message = data.get(message_offset..msg_end)?.to_vec();

    Some(Ed25519Verification { public_key, message })
}

/// Find the `ed25519` verify instruction in the transaction and confirm it
/// verifies `expected_message` against `expected_publisher`.
///
/// Scans the whole transaction: unrelated or malformed `ed25519` instructions
/// (for example other signatures bundled for multi-signature batching, or a
/// delegated fee payer) are skipped so that a valid publisher instruction that
/// appears later is still found. Returns [`FructusError::InvalidSignature`]
/// when the publisher's key is present but signs a different message, and
/// [`FructusError::SignatureMissing`] when no matching instruction exists.
pub fn verify_publisher_signature(
    instruction_sysvar: &AccountInfo,
    expected_publisher: &Pubkey,
    expected_message: &[u8; 32],
) -> Result<()> {
    let expected_pubkey = expected_publisher.to_bytes();
    let mut saw_wrong_message = false;
    let mut index: usize = 0;
    loop {
        let ix = match load_instruction_at_checked(index, instruction_sysvar) {
            Ok(ix) => ix,
            // End of the transaction's instruction list.
            Err(_) => break,
        };
        if ix.program_id.as_ref() == ed25519_program::ID.as_ref() {
            if let Some(parsed) = parse_ed25519_instruction(&ix.data) {
                if parsed.public_key == expected_pubkey {
                    if parsed.message.as_slice() == expected_message.as_slice() {
                        return Ok(());
                    }
                    saw_wrong_message = true;
                }
            }
        }
        index += 1;
    }
    Err(if saw_wrong_message {
        FructusError::InvalidSignature
    } else {
        FructusError::SignatureMissing
    }
    .into())
}
