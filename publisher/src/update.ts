import {
  Connection,
  Ed25519Program,
  Keypair,
  PublicKey,
  SYSVAR_INSTRUCTIONS_PUBKEY,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import { updateApyDiscriminator, updateMessage, writeU64LE } from "./message.js";

export interface UpdateParams {
  oracle: PublicKey;
  programId: PublicKey;
  publisher: Keypair;
  /** APY scaled by 1e6. */
  apy: bigint;
  /** Strictly greater than the oracle's current version. */
  version: bigint;
}

/** Build the anchor `update_apy` instruction without the anchor TS client. */
export function buildUpdateApyIx({ oracle, programId, apy, version }: UpdateParams): TransactionInstruction {
  const data = Buffer.concat([updateApyDiscriminator(), writeU64LE(apy), writeU64LE(version)]);
  return new TransactionInstruction({
    keys: [
      { pubkey: oracle, isSigner: false, isWritable: true },
      { pubkey: SYSVAR_INSTRUCTIONS_PUBKEY, isSigner: false, isWritable: false },
    ],
    programId,
    data,
  });
}

/** Build a signed transaction carrying the ed25519 verify + update_apy. */
export function buildUpdateTx(params: UpdateParams): Transaction {
  const { publisher, oracle, apy, version } = params;
  const message = updateMessage(oracle, apy, version);

  // web3.js >= 1.98 no longer exposes `Keypair.sign` — not in its types and not at
  // run time (`TypeError: publisher.sign is not a function`), which is why the
  // keeper was broken while its type-stripping tsx suite stayed green. The ed25519
  // program helper signs with the raw secret key and emits the verify instruction
  // in one call. Regression test: `test/update.test.ts`.
  const ed25519Ix = Ed25519Program.createInstructionWithPrivateKey({
    privateKey: publisher.secretKey,
    message,
  });

  const tx = new Transaction().add(ed25519Ix, buildUpdateApyIx(params));
  tx.feePayer = publisher.publicKey;
  return tx;
}

/** Submit the update transaction and return its signature. */
export async function submitUpdate(connection: Connection, params: UpdateParams): Promise<string> {
  const tx = buildUpdateTx(params);
  tx.recentBlockhash = (await connection.getLatestBlockhash()).blockhash;
  tx.sign(params.publisher);
  return await connection.sendRawTransaction(tx.serialize(), { skipPreflight: true });
}
