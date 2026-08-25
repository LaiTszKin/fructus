use trident_fuzz::fuzzing::*;

/// Storage for all account addresses used in fuzz testing.
///
/// This struct serves as a centralized repository for account addresses,
/// enabling their reuse across different instruction flows and test scenarios.
///
/// Docs: https://ackee.xyz/trident/docs/latest/trident-api-macro/trident-types/fuzz-accounts/
#[derive(Default)]
pub struct AccountAddresses {
    pub order_book: AddressStorage,

    pub market: AddressStorage,

    pub owner: AddressStorage,

    pub index_source: AddressStorage,

    pub position: AddressStorage,

    pub user_collateral: AddressStorage,

    pub cranker: AddressStorage,

    pub user: AddressStorage,

    pub vault: AddressStorage,

    pub user_ata: AddressStorage,

    pub collateral_mint: AddressStorage,

    pub system_program: AddressStorage,

    pub token_program: AddressStorage,

    pub oracle: AddressStorage,

    pub authority: AddressStorage,

    pub payer: AddressStorage,

    pub liquidator: AddressStorage,

    pub liquidator_collateral: AddressStorage,

    pub stake_pool: AddressStorage,

    pub instruction_sysvar: AddressStorage,
}
