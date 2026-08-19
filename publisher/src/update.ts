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
  const signature = publisher.sign(message); // 64-byte ed25519 signature

  const ed25519Ix = Ed25519Program.createInstructionWithPublicKey({
    publicKey: publisher.publicKey.toBytes(),
    message,
    signature,
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
