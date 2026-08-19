use fuzz_accounts::*;
use trident_fuzz::fuzzing::*;

mod fuzz_accounts;
mod types;
use types::*;

/// Fuzz target: the yield-oracle data module.
///
/// Drives `initialize` + publisher-signed `update_apy` sequences and checks the
/// stateful invariants after every transaction:
///   1. `apy <= MAX_APY` (bounds are enforced on every accepted update).
///   2. On an **accepted** update, `version` strictly increases and `apy` is set
///      to exactly the submitted value.
///   3. On a **rejected** update (stale version, tampered/missing signature),
///      the oracle state is left unchanged.
///   4. A message tampered after signing is never accepted.

const MAX_APY: u64 = 1_000_000;
const DOMAIN_SEPARATOR: &[u8] = b"fructus::update_apy";
const ORACLE_SEED: &[u8] = b"yield_oracle";
const INSTRUCTIONS_SYSVAR: Pubkey = pubkey!("Sysvar1nstructions1111111111111111111111111");

/// Canonical 32-byte update message, mirroring on-chain `update_message`.
fn update_message_bytes(oracle: &Pubkey, apy: u64, version: u64) -> solana_sdk::hash::Hash {
    let mut buf = Vec::with_capacity(DOMAIN_SEPARATOR.len() + 32 + 8 + 8);
    buf.extend_from_slice(DOMAIN_SEPARATOR);
    buf.extend_from_slice(oracle.as_ref());
    buf.extend_from_slice(&apy.to_le_bytes());
    buf.extend_from_slice(&version.to_le_bytes());
    solana_sdk::hash::hash(&buf)
}

fn read_oracle(trident: &mut Trident, oracle: &Pubkey) -> (u64, u64) {
    let account = trident.get_account(oracle);
    let data = account.data();
    let state = YieldOracle::deserialize(&mut &data[8..]).expect("oracle deserializes");
    (state.version, state.apy)
}

#[derive(FuzzTestMethods)]
struct FuzzTest {
    trident: Trident,
    fuzz_accounts: AccountAddresses,
    /// Fixed publisher keypair — signs the APY update payloads.
    publisher: Keypair,
}

#[flow_executor]
impl FuzzTest {
    fn new() -> Self {
        Self {
            trident: Trident::default(),
            fuzz_accounts: AccountAddresses::default(),
            publisher: Keypair::new(),
        }
    }

    #[init]
    fn start(&mut self) {
        // The pre-funded Trident payer doubles as the authority/payer.
        let authority = self.trident.payer().pubkey();
        let oracle = self.fuzz_accounts.oracle.insert(
            &mut self.trident,
            Some(PdaSeeds::new(&[ORACLE_SEED], fructus::program_id())),
        );

        let ix = fructus::InitializeInstruction::data(
            fructus::InitializeInstructionData::new(self.publisher.pubkey(), 10_000, 0),
        )
        .accounts(fructus::InitializeInstructionAccounts::new(oracle, authority))
        .instruction();

        self.trident.process_transaction(&[ix], Some("initialize"));
    }

    #[flow]
    fn update_apy(&mut self) {
        let Some(oracle) = self.fuzz_accounts.oracle.get(&mut self.trident) else {
            return;
        };

        // Snapshot the current on-chain state.
        let (cur_version, cur_apy) = read_oracle(&mut self.trident, &oracle);

        let apy: u64 = self.trident.random_from_range(0..=MAX_APY);
        let version: u64 = self.trident.random_from_range(0..1_000_000);

        // Occasionally sign a different message than the one the update
        // instruction carries, so the introspection can never match.
        let tamper = self.trident.random_bool();
        let signed_message = if tamper {
            update_message_bytes(&oracle, apy.wrapping_add(1), version)
        } else {
            update_message_bytes(&oracle, apy, version)
        };

        // ed25519 verify instruction + the anchor update_apy instruction.
        let signature = self.publisher.sign_message(signed_message.as_ref());
        let sig_bytes: [u8; 64] = signature.as_ref().try_into().expect("64-byte signature");
        let ed25519_ix =
            solana_sdk::ed25519_instruction::new_ed25519_instruction_with_signature(
                signed_message.as_ref(),
                &sig_bytes,
                &self.publisher.pubkey().to_bytes(),
            );
        let update_ix = fructus::UpdateApyInstruction::data(
            fructus::UpdateApyInstructionData::new(apy, version),
        )
        .accounts(fructus::UpdateApyInstructionAccounts::new(
            oracle,
            INSTRUCTIONS_SYSVAR,
        ))
        .instruction();

        let result = self
            .trident
            .process_transaction(&[ed25519_ix, update_ix], Some("update_apy"));

        // Re-read the state and assert the accept/reject invariants.
        let (new_version, new_apy) = read_oracle(&mut self.trident, &oracle);

        // Version never regresses, regardless of accept/reject.
        assert!(
            new_version >= cur_version,
            "version regressed: {} -> {}",
            cur_version,
            new_version
        );

        if result.is_success() {
            // A tampered message must never be accepted.
            assert!(!tamper, "a tampered message was accepted");
            // Acceptance implies a strictly-increasing version set to exactly
            // the submitted value, and the submitted APY.
            assert!(
                new_version > cur_version,
                "accepted update did not strictly increase version: {} -> {}",
                cur_version,
                new_version
            );
            assert_eq!(new_version, version, "accepted version mismatch");
            assert_eq!(new_apy, apy, "accepted apy mismatch");
        } else {
            // Rejection leaves the oracle unchanged.
            assert_eq!(new_version, cur_version, "rejected update changed version");
            assert_eq!(new_apy, cur_apy, "rejected update changed apy");
        }

        assert!(new_apy <= MAX_APY, "apy out of bounds: {}", new_apy);
    }
}

fn main() {
    FuzzTest::fuzz(1000, 100);
}
