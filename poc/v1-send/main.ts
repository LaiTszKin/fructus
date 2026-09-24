/**
 * Fructus — Stage-2 issue #18: the v1 send PoC.
 *
 * A minimal, self-contained walk that drives exactly ONE system transfer through a Transaction V1
 * (SIMD-0385 "Transaction V1") message on devnet, in the order `design.md` §1 prescribes:
 *
 *   1. keypair (`KEYPAIR`) and RPC URL (`RPC_URL`)
 *   2. GATE CHECK FIRST: `getAccountInfo(TXV1_GATE_ID)` — fail closed, never send when inactive
 *   3. build a v1 message with kit: one system transfer (recipient = sender), fee payer set,
 *      blockhash lifetime from the RPC
 *   4. ONE simulation with both limits maxed (kit's `estimateResourceLimitsFactory`), then the
 *      32 KiB page rounding of `loadedAccountsDataSizeLimit`, written via `setTransactionMessageConfig`
 *   5. compile → sign (`createKeyPairSignerFromBytes` over the web3 secret key) → send (base64) → confirm
 *   6. the pitfalls, with the live evidence gathered above
 *
 * The web3 instruction currency stays the SDK's public currency (`sdk/src/instructions.ts`), so the
 * PoC also carries the bridge #19 needs: `toKitInstruction()` maps a web3 `TransactionInstruction`
 * (programId/keys/data + signer/writable flags) onto a kit `IInstruction`
 * (programAddress/accounts/data + AccountRole).
 *
 * Modes:
 *   (default)      the full walk: send and confirm a real devnet transfer
 *   DRY_RUN=1      the same walk, stopping before `sendTransaction`. Step 4 still simulates, so the
 *   --dry-run      fee payer must still be funded (an unfunded simulation fails).
 *   --build-only   steps 1–3 plus the pitfall probes: the offline-buildable path, no simulation.
 *
 * Exit codes: 0 = ok; 2 = v1 gate inactive (fail closed); 3 = fee payer unfunded / send blocked;
 * 1 = any other failure.
 */
import { readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';

import {
    AccountRole,
    address,
    appendTransactionMessageInstruction,
    compileTransaction,
    compileTransactionMessage,
    createKeyPairSignerFromBytes,
    createSolanaRpc,
    createTransactionMessage,
    devnet,
    estimateResourceLimitsFactory,
    fillTransactionMessageProvisoryResourceLimits,
    getBase64EncodedWireTransaction,
    getCompiledTransactionMessageEncoder,
    getSignatureFromTransaction,
    pipe,
    setTransactionMessageConfig,
    setTransactionMessageFeePayerSigner,
    setTransactionMessageLifetimeUsingBlockhash,
    signTransactionWithSigners,
} from '@solana/kit';
import { ComputeBudgetProgram, Connection, Keypair, PublicKey, SystemProgram } from '@solana/web3.js';
import type { TransactionInstruction } from '@solana/web3.js';

// ---------------------------------------------------------------------------------------------
// constants
// ---------------------------------------------------------------------------------------------

/** The devnet-active SIMD-0385 feature account, as measured (see .plan/.../AMENDMENTS.md). */
const TXV1_GATE_ID = 'txv1aq4pp281K9um3tnPgkfX8UqtFT6wcVW3hNezGLL';
/** The 41-character literal the plan originally carried. Not a valid pubkey — printed as evidence. */
const TXV1_GATE_ID_PLAN_TYPO = 'txv1aq4pp281K9um3pgkfX8UqtFT6wcVW3hNezGLL';
/** v1 `loadedAccountsDataSizeLimit` is priced in 32 KiB pages; round up to the next whole page. */
const V1_PAGE_BYTES = 32 * 1024;
/** The maximum size of a serialized v1 message (kit: `V1_TRANSACTION_SIZE_LIMIT`). */
const V1_MAX_MESSAGE_BYTES = 4096;
/** kit's estimator maxes compute units at 1,400,000 (= Agave's MAX_COMPUTE_UNIT_LIMIT). */
const MAX_COMPUTE_UNIT_LIMIT = 1_400_000;
/** At or below this many bytes an RPC will still accept a base58 wire transaction. */
const LEGACY_PACKET_DATA_SIZE = 1232;

// ---------------------------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------------------------

const args = process.argv.slice(2);
const dryRun = process.env.DRY_RUN === '1' || args.includes('--dry-run');
const buildOnly = process.env.BUILD_ONLY === '1' || args.includes('--build-only');

function step(n: number, title: string): void {
    console.log(`\n──── step ${n} — ${title} ────`);
}
function kv(label: string, value: unknown): void {
    console.log(
        `  ${label.padEnd(34)} ${typeof value === 'string' ? value : JSON.stringify(value, bigintReplacer)}`,
    );
}
function bigintReplacer(_key: string, value: unknown): unknown {
    return typeof value === 'bigint' ? `${value}n` : value;
}
function expandTilde(path: string): string {
    return path.startsWith('~') ? join(homedir(), path.slice(1)) : path;
}

/** Next multiple of 32 KiB (0 → 0, 1 → 32768, 32768 → 32768, 32769 → 65536). */
function roundUpToPage(bytes: number): number {
    return bytes === 0 ? 0 : Math.ceil(bytes / V1_PAGE_BYTES) * V1_PAGE_BYTES;
}

/** web3 `isSigner`/`isWritable` flags → kit `AccountRole`. */
function accountRole(isSigner: boolean, isWritable: boolean): AccountRole {
    if (isSigner) return isWritable ? AccountRole.WRITABLE_SIGNER : AccountRole.READONLY_SIGNER;
    return isWritable ? AccountRole.WRITABLE : AccountRole.READONLY;
}

/** The web3 → kit instruction bridge #19's `buildV1Message` needs. */
function toKitInstruction(instruction: TransactionInstruction) {
    return {
        programAddress: address(instruction.programId.toBase58()),
        accounts: instruction.keys.map(key => ({
            address: address(key.pubkey.toBase58()),
            role: accountRole(key.isSigner, key.isWritable),
        })),
        data: new Uint8Array(instruction.data),
    };
}

type SimOutcome = { ok: true; value: unknown } | { ok: false; error: string };

/** Simulate a signed transaction exactly as built (nothing maxed) and report the raw outcome. */
async function simulateRaw(rpc: ReturnType<typeof createSolanaRpc>, transaction: unknown): Promise<SimOutcome> {
    const wire = getBase64EncodedWireTransaction(
        transaction as Parameters<typeof getBase64EncodedWireTransaction>[0],
    );
    try {
        const response = await rpc
            .simulateTransaction(wire, {
                encoding: 'base64',
                replaceRecentBlockhash: true,
                sigVerify: false,
            })
            .send();
        return { ok: true, value: response.value };
    } catch (e) {
        return { ok: false, error: e instanceof Error ? `${e.name}: ${e.message}` : String(e) };
    }
}

/** Flatten an error's `cause` chain — kit wraps RPC failures several levels deep. */
function causeChain(error: unknown): string[] {
    const chain: string[] = [];
    let cause: unknown = (error as { cause?: unknown })?.cause;
    for (let depth = 0; cause && depth < 4; depth++) {
        chain.push(cause instanceof Error ? `${cause.name}: ${cause.message}` : JSON.stringify(cause, bigintReplacer));
        cause = (cause as { cause?: unknown })?.cause;
    }
    return chain;
}
/** True when the failure is "the fee payer has no lamports", not a code problem. */
function isFundingFailure(error: unknown): boolean {
    const text = causeChain(error).join(' | ');
    return /no record of a prior credit|insufficient|debit an account|AccountNotFound/i.test(text);
}

// ---------------------------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------------------------

async function main(): Promise<void> {
    console.log('Fructus v1 send PoC — Transaction V1 (SIMD-0385) on devnet');
    kv('dryRun', dryRun);
    kv('buildOnly', buildOnly);

    // ── step 1 — keypair + RPC ───────────────────────────────────────────────────────────────
    step(1, 'load the keypair and the RPC URL');
    const keypairPath = resolve(expandTilde(process.env.KEYPAIR ?? '~/.config/solana/id.json'));
    const rpcUrl = process.env.RPC_URL ?? 'https://api.devnet.solana.com';
    const amountLamports = BigInt(process.env.AMOUNT_LAMPORTS ?? '1');

    const secretKey = new Uint8Array(JSON.parse(readFileSync(keypairPath, 'utf8')) as number[]);
    const keypair = Keypair.fromSecretKey(secretKey);
    const connection = new Connection(rpcUrl, 'confirmed');
    const rpc = createSolanaRpc(devnet(rpcUrl));
    const accountInfo = await connection.getAccountInfo(keypair.publicKey);
    const balance = await connection.getBalance(keypair.publicKey);

    kv('KEYPAIR', keypairPath);
    kv('RPC_URL', rpcUrl);
    kv('pubkey', keypair.publicKey.toBase58());
    kv('balance (lamports)', balance);
    kv('account exists on chain', accountInfo !== null);

    // ── step 2 — gate check first (fail closed) ──────────────────────────────────────────────
    step(2, 'GATE CHECK: getAccountInfo on the devnet-active SIMD-0385 feature account');
    const gateAccount = await connection.getAccountInfo(new PublicKey(TXV1_GATE_ID), 'confirmed');
    kv('gate id (measured)', TXV1_GATE_ID);
    if (!gateAccount) {
        console.error(
            `\n  FAIL — the v1 gate is INACTIVE: feature account ${TXV1_GATE_ID} is absent on ${rpcUrl}.` +
                '\n  Refusing to build or send anything (fail closed).',
        );
        process.exit(2);
    }
    kv('gate account owner', gateAccount.owner.toBase58());
    kv('gate account lamports', gateAccount.lamports);
    kv('gate account data (base64)', gateAccount.data.toString('base64'));
    kv('gate account space', gateAccount.data.length);

    // The plan's 41-character literal is not a valid pubkey — prove the amendment live.
    try {
        new PublicKey(TXV1_GATE_ID_PLAN_TYPO);
        kv('plan typo literal (41 chars)', 'UNEXPECTEDLY valid');
    } catch (e) {
        kv('plan typo literal (41 chars)', `invalid — ${e instanceof Error ? e.message : String(e)}`);
    }

    // ── step 3 — build the v1 message ────────────────────────────────────────────────────────
    step(3, 'build a v1 message: one system transfer (recipient = sender)');
    const signer = await createKeyPairSignerFromBytes(secretKey);
    const { blockhash, lastValidBlockHeight } = await connection.getLatestBlockhash('confirmed');
    kv('blockhash', blockhash);
    kv('lastValidBlockHeight', lastValidBlockHeight);

    const transfer = SystemProgram.transfer({
        fromPubkey: keypair.publicKey,
        lamports: Number(amountLamports),
        toPubkey: keypair.publicKey, // recipient = sender
    });
    kv('web3 transfer ix data (hex)', Buffer.from(transfer.data).toString('hex'));
    kv(
        'web3 transfer ix keys',
        transfer.keys.map(k => `${k.pubkey.toBase58().slice(0, 8)}… signer=${k.isSigner} writable=${k.isWritable}`),
    );

    const kitTransfer = toKitInstruction(transfer);
    kv('kit ix programAddress', kitTransfer.programAddress);
    kv('kit ix accounts', kitTransfer.accounts.map(a => `${a.address.slice(0, 8)}… role=${AccountRole[a.role]}`));

    const baseMessage = pipe(
        createTransactionMessage({ version: 1 }),
        m => setTransactionMessageFeePayerSigner(signer, m),
        m =>
            setTransactionMessageLifetimeUsingBlockhash(
                { blockhash: blockhash as never, lastValidBlockHeight: BigInt(lastValidBlockHeight) },
                m,
            ),
    );
    // Provisory limits (0) keep the config mask stable while the message is being built; the
    // estimator replaces them with measured values.
    const provisory = fillTransactionMessageProvisoryResourceLimits(
        appendTransactionMessageInstruction(kitTransfer, baseMessage),
    );
    const provisoryCompiled = compileTransactionMessage(provisory);
    kv('provisory configMask', `0b${provisoryCompiled.configMask.toString(2)}`);
    kv('provisory configValues', provisoryCompiled.configValues);
    kv('staticAccounts (deduped)', provisoryCompiled.staticAccounts);
    kv('numStaticAccounts', provisoryCompiled.numStaticAccounts);
    kv('compiled message keys', Object.keys(provisoryCompiled).sort());

    // ── step 4 — ONE simulation, then page-round the loaded-data limit ───────────────────────
    step(4, 'ONE simulation with both limits maxed, then the 32 KiB page rounding');
    let blocked: unknown = null;
    let limits: { computeUnitLimit: number; loadedAccountsDataSizeLimit: number } | null = null;
    let configured: typeof provisory | null = null;

    // The rounding contract the kit estimator does NOT apply (it returns the raw simulated value).
    kv(
        'roundUpToPage self-check',
        [0, 1, 32768, 32769, 65536].map(n => `${n} → ${roundUpToPage(n)}`).join(', '),
    );

    if (buildOnly) {
        console.log('  build-only — the simulation (and everything after it) is skipped.');
    } else {
        const estimate = estimateResourceLimitsFactory({ rpc });
        try {
            const start = Date.now();
            limits = await estimate(provisory);
            kv('simulation wall time (ms)', Date.now() - start);
            kv('estimated computeUnitLimit', limits.computeUnitLimit);
            kv('simulated loadedAccountsDataSizeLimit', limits.loadedAccountsDataSizeLimit);
            const roundedDataLimit = roundUpToPage(limits.loadedAccountsDataSizeLimit);
            kv(
                'page-rounded loadedAccountsDataSizeLimit',
                `${roundedDataLimit} (${roundedDataLimit / V1_PAGE_BYTES} page(s))`,
            );

            configured = setTransactionMessageConfig(
                {
                    computeUnitLimit: limits.computeUnitLimit,
                    loadedAccountsDataSizeLimit: roundedDataLimit,
                    priorityFeeLamports: 0n, // a TOTAL in lamports, not a price per compute unit
                },
                provisory,
            );
            const configuredCompiled = compileTransactionMessage(configured);
            kv('final configMask', `0b${configuredCompiled.configMask.toString(2)}`);
            kv('final configValues', configuredCompiled.configValues);
        } catch (e) {
            blocked = e;
            console.error(`\n  the estimator's simulation FAILED — ${e instanceof Error ? e.message : String(e)}`);
            for (const [depth, detail] of causeChain(e).entries()) console.error(`  cause[${depth}] — ${detail}`);
        }
    }

    // ── step 5 — compile, sign, send, confirm ────────────────────────────────────────────────
    step(5, `compile → sign${dryRun || buildOnly || blocked ? ' (no send)' : ' → send → confirm'}`);
    const buildable = configured ?? provisory; // build-only / blocked runs sign the provisory message
    const signed = await signTransactionWithSigners([signer], compileTransaction(buildable));
    const wireTransaction = getBase64EncodedWireTransaction(signed);
    const signature = getSignatureFromTransaction(signed);
    const wireBytes = Math.floor((wireTransaction.length * 3) / 4);

    kv('compiled message bytes', `${signed.messageBytes.length} / ${V1_MAX_MESSAGE_BYTES}`);
    kv('wire bytes (base64)', wireBytes);
    kv('wire encoding', 'base64 (mandatory above 1232 bytes; kit always sends base64)');
    kv('signature (pre-send)', signature);

    let confirmedSignature: string | null = null;
    if (buildOnly || blocked || dryRun) {
        console.log(
            buildOnly
                ? '  build-only — built and signed, never sent (the provisory message budgets 0 units).'
                : blocked
                  ? '  BLOCKED — built and signed, never sent (see the blocker below).'
                  : '  DRY RUN — built and signed, never sent.',
        );
    } else {
        const sendStart = Date.now();
        const sent = await rpc
            .sendTransaction(wireTransaction, { encoding: 'base64', preflightCommitment: 'confirmed' })
            .send();
        kv('rpc.sendTransaction →', sent);
        kv('send wall time (ms)', Date.now() - sendStart);

        const confirmation = await connection.confirmTransaction(
            { blockhash, lastValidBlockHeight, signature: sent },
            'confirmed',
        );
        if (confirmation.value.err) {
            throw new Error(`transaction ${sent} landed with an error: ${JSON.stringify(confirmation.value.err)}`);
        }
        confirmedSignature = sent;
        kv('confirmed signature', sent);
        kv('explorer', `https://explorer.solana.com/tx/${sent}?cluster=devnet`);
        kv('lamports transferred', `${amountLamports} (self-transfer, plus the 5000-lamport signature fee)`);
    }

    // ── step 6 — the pitfalls, with live probes ──────────────────────────────────────────────
    step(6, 'pitfall probes (live, non-fatal)');

    // Probe A — a v1 message with NO config that carries a ComputeBudget SetComputeUnitLimit
    // instruction: if unset limits really are zero and ComputeBudget really is a no-op, this must be
    // refused even though it asks for the maximum 1,400,000 units the legacy way.
    const probeAMessage = pipe(
        appendTransactionMessageInstruction(
            toKitInstruction(ComputeBudgetProgram.setComputeUnitLimit({ units: MAX_COMPUTE_UNIT_LIMIT })),
            baseMessage,
        ),
        m => appendTransactionMessageInstruction(kitTransfer, m),
    ); // deliberately not configured
    kv('probe A configMask', `0b${compileTransactionMessage(probeAMessage).configMask.toString(2)}`);
    const probeATx = await signTransactionWithSigners([signer], compileTransaction(probeAMessage));
    const probeA = await simulateRaw(rpc, probeATx);
    kv('probe A simulate →', probeA.ok ? probeA.value : probeA.error);

    // Probe B — the same message with a duplicate static address spliced into the compiled account
    // list: a v1 message's address list must be deduplicated by the sender.
    let probeB: SimOutcome;
    try {
        const dupedMessage = {
            ...provisoryCompiled,
            numStaticAccounts: provisoryCompiled.numStaticAccounts + 1,
            staticAccounts: [...provisoryCompiled.staticAccounts, provisoryCompiled.staticAccounts[0]],
        };
        const dupedBytes = getCompiledTransactionMessageEncoder().encode(dupedMessage);
        probeB = await simulateRaw(rpc, {
            ...signed,
            messageBytes: dupedBytes as unknown as typeof signed.messageBytes,
        });
    } catch (e) {
        probeB = { ok: false, error: e instanceof Error ? `${e.name}: ${e.message}` : String(e) };
    }
    kv('probe B simulate →', probeB.ok ? probeB.value : probeB.error);

    // ── pitfalls, as validated above ─────────────────────────────────────────────────────────
    console.log('\n──── pitfalls, with the evidence this run produced ────');
    const pitfalls = [
        [
            'unset v1 limits are ZERO, never defaults — kit writes only what the config mask carries:',
            `provisory configValues ${JSON.stringify(provisoryCompiled.configValues)}` +
                (configured ? `, measured ${JSON.stringify(compileTransactionMessage(configured).configValues)}` : ''),
        ],
        [
            'the priority fee is a TOTAL in lamports (V1TransactionConfig.priorityFeeLamports), not a price',
            'per compute unit — this message pays 0n; a ComputeBudget price instruction is a no-op (probe A)',
        ],
        [
            `base64 is mandatory above ${LEGACY_PACKET_DATA_SIZE} bytes — measured wire size`,
            `${wireBytes} bytes, submitted as base64 regardless; kit's sendTransaction only ever encodes base64`,
        ],
        [
            'ComputeBudget instructions are no-ops under v1 — never include them: probe A asks for',
            `${MAX_COMPUTE_UNIT_LIMIT} units that way and is refused anyway`,
        ],
        [
            'v1 has no ALT support — the compiled v1 message carries',
            `staticAccounts only, no addressTableLookups field (keys: ${Object.keys(provisoryCompiled).sort().join(', ')})`,
        ],
        [
            'duplicate addresses are rejected by the protocol — kit merges repeated references into one',
            `static account (${provisoryCompiled.numStaticAccounts} for 3 role references: the fee payer, the sender and the recipient are the same key); a hand-spliced duplicate (probe B) is refused`,
        ],
    ];
    for (const [a, b] of pitfalls) console.log(`  • ${a}\n      ${b}`);

    // ── outcome ──────────────────────────────────────────────────────────────────────────────
    if (blocked) {
        const funding = isFundingFailure(blocked);
        console.error(
            `\nBLOCKED — ${funding ? 'the fee payer is unfunded' : 'the walk stopped'} before the send leg.` +
                (funding
                    ? `\n  ${keypair.publicKey.toBase58()} holds ${balance} lamports, so the step-4 simulation cannot succeed.` +
                      '\n  Fund it (solana airdrop 2 --url devnet, or https://faucet.solana.com) and re-run.'
                    : ''),
        );
        process.exit(funding ? 3 : 1);
    }

    console.log(
        '\nOK' +
            (confirmedSignature
                ? ` — confirmed signature ${confirmedSignature}`
                : buildOnly
                  ? ' (build-only: the message is built, signed and never sent)'
                  : ' (dry run: no signature)'),
    );
}

main().catch(error => {
    const message = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
    console.error(`\nFAIL — ${message}`);
    for (const [depth, detail] of causeChain(error).entries()) console.error(`  cause[${depth}] — ${detail}`);
    if (error instanceof Error && error.stack) console.error(error.stack.split('\n').slice(0, 6).join('\n'));
    process.exit(1);
});
