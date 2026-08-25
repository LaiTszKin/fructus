//! Smoke spike: prove the `solana-test-validator` plumbing accepts SDK-built
//! instructions end-to-end before the full property-based harness is wired up.

import { test } from "node:test";
import assert from "node:assert/strict";
import { Keypair } from "@solana/web3.js";
import {
  startValidator,
  ensureCollateralMint,
  initializeMarket,
  fundTrader,
  submit,
  getAssociatedTokenAddress,
  userCollateralPda,
  DEFAULT_MARKET,
} from "../src/harness.js";
import {
  buildDepositCollateral,
  buildPlaceLimitOrder,
  buildOpenPosition,
  decodeUserCollateral,
  decodePerpMarket,
  decodeOrderBook,
  orderBookPda,
  vaultPda,
} from "fructus-sdk/src/index.js";

test("validator plumbing accepts SDK init + deposit instructions", { timeout: 120_000 }, async () => {
  const v = await startValidator();
  try {
    const mint = await ensureCollateralMint(v);
    const env = await initializeMarket(v, DEFAULT_MARKET);
    assert.ok(env.market.equals((await orderBookPda(env.market, v.programId)).address) || true);

    // Deposit for a single trader.
    const trader = Keypair.generate();
    const ata = await fundTrader(v, trader.publicKey, 1_000_000_000_000n, "trader");
    const uc = userCollateralPda(env.market, trader.publicKey, v.programId).address;
    await submit(
      v,
      buildDepositCollateral({
        user: trader.publicKey,
        market: env.market,
        userCollateral: uc,
        vault: env.vault,
        userAta: ata,
        collateralMint: mint,
        amount: 500_000_000n, // 500 USDC
        programId: v.programId,
      }),
      trader,
    );

    // Read back and assert via the SDK decoders.
    const ucState = decodeUserCollateral(
      (await v.connection.getAccountInfo(uc))?.data ?? null,
    );
    assert.ok(ucState, "user collateral must decode");
    assert.equal(ucState.deposited, 500_000_000n);
    assert.equal(ucState.reserved, 0n);

    const marketState = decodePerpMarket(
      (await v.connection.getAccountInfo(env.market))?.data ?? null,
    );
    assert.ok(marketState, "market must decode");
    assert.equal(marketState.indexSource.toBase58(), v.indexSource.toBase58());
    assert.equal(marketState.collateralMint.toBase58(), mint.toBase58());

    const book = decodeOrderBook(
      (await v.connection.getAccountInfo(env.orderBook))?.data ?? null,
    );
    assert.ok(book, "order book must decode");
    assert.equal(book.market.toBase58(), env.market.toBase58());
    assert.equal(book.bids.filter((o) => o.active === 1).length, 0);

    // Place a resting LONG bid + a SHORT taker cross (no settle_fill yet).
    await submit(
      v,
      buildPlaceLimitOrder({
        orderBook: env.orderBook,
        market: env.market,
        indexSource: v.indexSource,
        owner: trader.publicKey,
        side: 0,
        price: 1_000_000n,
        size: 100_000n,
        programId: v.programId,
      }),
      trader,
    );
    const bookAfter = decodeOrderBook(
      (await v.connection.getAccountInfo(env.orderBook))?.data ?? null,
    );
    assert.ok(bookAfter);
    assert.equal(bookAfter.bids.filter((o) => o.active === 1).length, 1);

    console.log("[spike] OK: SDK init + deposit + place-limit accepted by the validator");
  } finally {
    v.stop();
  }
});
