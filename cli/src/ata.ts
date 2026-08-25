//! Deterministic associated-token-account derivation (the CLI uses this instead
//! of `@solana/spl-token` to stay dependency-free).

import { PublicKey } from "@solana/web3.js";
import { ASSOCIATED_TOKEN_PROGRAM_ID, TOKEN_PROGRAM_ID } from "fructus-sdk/src/index.js";

/**
 * The associated token account for `owner` holding `mint`, derived identically
 * to the SPL token program's `get_associated_token_address`.
 */
export function deriveAta(owner: PublicKey, mint: PublicKey): PublicKey {
  const [address] = PublicKey.findProgramAddressSync(
    [owner.toBuffer(), TOKEN_PROGRAM_ID.toBuffer(), mint.toBuffer()],
    ASSOCIATED_TOKEN_PROGRAM_ID,
  );
  return address;
}
