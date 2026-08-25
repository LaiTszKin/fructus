import { test } from "node:test";
import assert from "node:assert/strict";
import { PublicKey } from "@solana/web3.js";
import { mid } from "../src/orderbook.js";
import {
  PerpMarket,
  Position,
  UserCollateralLayout,
  YieldOracleLayout,
  OrderLayout,
  OutEventLayout,
  ObservationLayout,
  OrderBookLayout,
  PERP_MARKET_LEN,
  POSITION_LEN,
  USER_COLLATERAL_LEN,
  YIELD_ORACLE_LEN,
  ORDER_BOOK_LEN,
  ORDER_LEN,
  OUT_EVENT_LEN,
  OBSERVATION_LEN,
  DISCRIMINATOR,
} from "../src/account/layout.js";
import {
  decodeOrderBook,
  decodePerpMarket,
  decodePosition,
  decodeUserCollateral,
  decodeYieldOracle,
} from "../src/account/decode.js";

// Cross-language layout lock (R-SDK3b): the account-layout offset/size
// constants must be byte-identical with `programs/fructus/src/state.rs`, whose
// unit tests pin `PerpMarket::LEN == 197`, `Position::LEN == 138`,
// `UserCollateral::LEN == 17`, `Order::LEN == 64`, `OutEvent::LEN == 112`,
// `Observation::LEN == 32`, `OrderBook::LEN == 23_128`. Field offsets follow the
// borsh / `#[repr(C)]` field order declared in `state.rs`.

test("pinned payload sizes match the on-chain program", () => {
  assert.equal(PERP_MARKET_LEN, 197);
  assert.equal(POSITION_LEN, 138);
  assert.equal(USER_COLLATERAL_LEN, 17);
  assert.equal(YIELD_ORACLE_LEN, 97);
  assert.equal(ORDER_LEN, 64);
  assert.equal(OUT_EVENT_LEN, 112);
  assert.equal(OBSERVATION_LEN, 32);
  assert.equal(ORDER_BOOK_LEN, 23_128);
});

test("PerpMarket offsets sum to LEN 197", () => {
  const o = PerpMarket;
  // indices + sizes in field order.
  const order: [number, number][] = [
    [o.indexSource, 32],
    [o.collateralMint, 32],
    [o.fundingK, 8],
    [o.maxFunding, 8],
    [o.fundingEpochSlots, 8],
    [o.initialMarginBps, 2],
    [o.maintenanceMarginBps, 2],
    [o.authority, 32],
    [o.vault, 32],
    [o.fundingEpoch, 8],
    [o.indexN, 8],
    [o.indexD, 8],
    [o.fundingAccumulator, 16],
    [o.bump, 1],
  ];
  let cursor = 0;
  for (const [off, size] of order) {
    assert.equal(off, cursor, `PerpMarket field offset ${off} != running offset ${cursor}`);
    cursor += size;
  }
  assert.equal(cursor, PERP_MARKET_LEN);
  // Spot-check the documented funding-field offsets (R-F4 fields).
  assert.equal(o.fundingEpoch, 156);
  assert.equal(o.indexN, 164);
  assert.equal(o.indexD, 172);
  assert.equal(o.fundingAccumulator, 180);
});

test("Position offsets sum to LEN 138", () => {
  const o = Position;
  const order: [number, number][] = [
    [o.market, 32],
    [o.owner, 32],
    [o.side, 1],
    [o.notional, 8],
    [o.entryN, 16],
    [o.entryD, 16],
    [o.collateral, 8],
    [o.lastFundingEpoch, 8],
    [o.closedNotional, 8],
    [o.openSlot, 8],
    [o.bump, 1],
  ];
  let cursor = 0;
  for (const [off, size] of order) {
    assert.equal(off, cursor, `Position field offset ${off} != running offset ${cursor}`);
    cursor += size;
  }
  assert.equal(cursor, POSITION_LEN);
  assert.equal(o.closedNotional, 121);
});

test("UserCollateral and YieldOracle offsets", () => {
  let cursor = 0;
  for (const [off, size] of [
    [UserCollateralLayout.deposited, 8],
    [UserCollateralLayout.reserved, 8],
    [UserCollateralLayout.bump, 1],
  ]) {
    assert.equal(off, cursor);
    cursor += size;
  }
  assert.equal(cursor, USER_COLLATERAL_LEN);

  cursor = 0;
  for (const [off, size] of [
    [YieldOracleLayout.apy, 8],
    [YieldOracleLayout.version, 8],
    [YieldOracleLayout.lastUpdateSlot, 8],
    [YieldOracleLayout.publisher, 32],
    [YieldOracleLayout.authority, 32],
    [YieldOracleLayout.staleAfterSlots, 8],
    [YieldOracleLayout.bump, 1],
  ]) {
    assert.equal(off, cursor, `YieldOracle field offset ${off} != ${cursor}`);
    cursor += size;
  }
  assert.equal(cursor, YIELD_ORACLE_LEN);
});

test("Order / OutEvent / Observation and OrderBook array offsets", () => {
  assert.equal(OrderLayout.owner + 32 + 8 + 8 + 8 + 1 + 7, ORDER_LEN);
  assert.equal(OutEventLayout.side + 1 + 5, OUT_EVENT_LEN);
  assert.equal(ObservationLayout.cumulativeMid + 16, OBSERVATION_LEN);

  // OrderBook header and array bases (bids -> asks -> events -> observations).
  assert.equal(OrderBookLayout.headerLen, 88);
  assert.equal(OrderBookLayout.bids, 88);
  assert.equal(OrderBookLayout.asks, OrderBookLayout.bids + 64 * ORDER_LEN);
  assert.equal(OrderBookLayout.events, OrderBookLayout.asks + 64 * ORDER_LEN);
  assert.equal(
    OrderBookLayout.observations,
    OrderBookLayout.events + 128 * OUT_EVENT_LEN,
  );
  assert.equal(
    OrderBookLayout.observations + 16 * OBSERVATION_LEN,
    ORDER_BOOK_LEN,
  );
});

// --- Decoder round-trip ---

const M = PublicKey.unique();
const D = DISCRIMINATOR;

test("decodePerpMarket reads every field incl the funding state", () => {
  const buf = Buffer.alloc(D + PERP_MARKET_LEN);
  buf.set(M.toBuffer(), D + PerpMarket.indexSource);
  buf.set(M.toBuffer(), D + PerpMarket.collateralMint);
  buf.writeBigUInt64LE(100_000n, D + PerpMarket.fundingK);
  buf.writeBigUInt64LE(10_000n, D + PerpMarket.maxFunding);
  buf.writeBigUInt64LE(1_000n, D + PerpMarket.fundingEpochSlots);
  buf.writeUInt16LE(1_000, D + PerpMarket.initialMarginBps);
  buf.writeUInt16LE(500, D + PerpMarket.maintenanceMarginBps);
  buf.set(M.toBuffer(), D + PerpMarket.authority);
  buf.set(M.toBuffer(), D + PerpMarket.vault);
  buf.writeBigUInt64LE(7n, D + PerpMarket.fundingEpoch);
  buf.writeBigUInt64LE(111n, D + PerpMarket.indexN);
  buf.writeBigUInt64LE(222n, D + PerpMarket.indexD);
  // funding_accumulator = -42 as i128 LE (16 bytes two's-complement).
  const neg = 1n << 128n;
  const acc = neg - 42n;
  for (let i = 0; i < 16; i++) {
    buf[D + PerpMarket.fundingAccumulator + i] = Number((acc >> BigInt(8 * i)) & 0xffn);
  }
  buf[D + PerpMarket.bump] = 255;

  const m = decodePerpMarket(buf)!;
  assert.equal(m.indexSource.toBase58(), M.toBase58());
  assert.equal(m.collateralMint.toBase58(), M.toBase58());
  assert.equal(m.fundingK, 100_000n);
  assert.equal(m.maxFunding, 10_000n);
  assert.equal(m.fundingEpochSlots, 1_000n);
  assert.equal(m.initialMarginBps, 1_000);
  assert.equal(m.maintenanceMarginBps, 500);
  assert.equal(m.fundingEpoch, 7n);
  assert.equal(m.indexN, 111n);
  assert.equal(m.indexD, 222n);
  assert.equal(m.fundingAccumulator, -42n);
  assert.equal(m.bump, 255);
});

test("decodePosition reads the closed_notional + entry sums", () => {
  const buf = Buffer.alloc(D + POSITION_LEN);
  buf.set(M.toBuffer(), D + Position.market);
  buf.set(M.toBuffer(), D + Position.owner);
  buf[D + Position.side] = 1;
  buf.writeBigUInt64LE(5_000_000n, D + Position.notional);
  // entry_n = 123456789abcdef0 (u128 LE).
  const n = 0x0123456789abcdefn;
  for (let i = 0; i < 16; i++) buf[D + Position.entryN + i] = Number((n >> BigInt(8 * i)) & 0xffn);
  for (let i = 0; i < 16; i++) buf[D + Position.entryD + i] = Number((n >> BigInt(8 * i)) & 0xffn);
  buf.writeBigUInt64LE(1_000_000n, D + Position.collateral);
  buf.writeBigUInt64LE(42n, D + Position.lastFundingEpoch);
  buf.writeBigUInt64LE(900_000n, D + Position.closedNotional);
  buf.writeBigUInt64LE(7n, D + Position.openSlot);
  buf[D + Position.bump] = 200;

  const p = decodePosition(buf)!;
  assert.equal(p.side, 1);
  assert.equal(p.notional, 5_000_000n);
  assert.equal(p.entryN, n);
  assert.equal(p.entryD, n);
  assert.equal(p.collateral, 1_000_000n);
  assert.equal(p.lastFundingEpoch, 42n);
  assert.equal(p.closedNotional, 900_000n);
  assert.equal(p.openSlot, 7n);
  assert.equal(p.bump, 200);
});

test("decodeUserCollateral reads deposited/reserved/bump", () => {
  const buf = Buffer.alloc(D + USER_COLLATERAL_LEN);
  buf.writeBigUInt64LE(1_234_567n, D + UserCollateralLayout.deposited);
  buf.writeBigUInt64LE(0n, D + UserCollateralLayout.reserved);
  buf[D + UserCollateralLayout.bump] = 9;
  const uc = decodeUserCollateral(buf)!;
  assert.equal(uc.deposited, 1_234_567n);
  assert.equal(uc.reserved, 0n);
  assert.equal(uc.bump, 9);
});

test("decodeYieldOracle reads apy/version/last_update_slot/stale_after_slots", () => {
  const buf = Buffer.alloc(D + YIELD_ORACLE_LEN);
  buf.writeBigUInt64LE(71_840n, D + YieldOracleLayout.apy);
  buf.writeBigUInt64LE(3n, D + YieldOracleLayout.version);
  buf.writeBigUInt64LE(123_456n, D + YieldOracleLayout.lastUpdateSlot);
  buf.set(M.toBuffer(), D + YieldOracleLayout.publisher);
  buf.set(M.toBuffer(), D + YieldOracleLayout.authority);
  buf.writeBigUInt64LE(42_000n, D + YieldOracleLayout.staleAfterSlots);
  buf[D + YieldOracleLayout.bump] = 1;
  const o = decodeYieldOracle(buf)!;
  assert.equal(o.apy, 71_840n);
  assert.equal(o.version, 3n);
  assert.equal(o.lastUpdateSlot, 123_456n);
  assert.equal(o.staleAfterSlots, 42_000n);
});

test("decodeOrderBook reads the header and array elements", () => {
  const buf = Buffer.alloc(D + ORDER_BOOK_LEN);
  buf.writeBigUInt64LE(9n, D + OrderBookLayout.nextSeq);
  buf.writeBigUInt64LE(1_050_000n, D + OrderBookLayout.bestBid);
  buf.writeBigUInt64LE(1_100_000n, D + OrderBookLayout.bestAsk);
  buf.set(M.toBuffer(), D + OrderBookLayout.market);
  buf[D + OrderBookLayout.bump] = 3;
  // One active bid at bids[0].
  const b0 = D + OrderBookLayout.bids;
  buf.set(M.toBuffer(), b0 + OrderLayout.owner);
  buf.writeBigUInt64LE(1_050_000n, b0 + OrderLayout.price);
  buf.writeBigUInt64LE(1_000_000n, b0 + OrderLayout.size);
  buf.writeBigUInt64LE(1n, b0 + OrderLayout.seq);
  buf[b0 + OrderLayout.active] = 1;
  // One observation at observations[0].
  const o0 = D + OrderBookLayout.observations;
  buf.writeBigUInt64LE(100n, o0 + ObservationLayout.slot);
  buf.writeBigUInt64LE(1_075_000n, o0 + ObservationLayout.mid);
  buf.writeBigUInt64LE(2_000_000n, o0 + ObservationLayout.cumulativeMid);

  const ob = decodeOrderBook(buf)!;
  assert.equal(ob.nextSeq, 9n);
  assert.equal(ob.bestBid, 1_050_000n);
  assert.equal(ob.bestAsk, 1_100_000n);
  assert.equal(ob.market.toBase58(), M.toBase58());
  assert.equal(ob.bump, 3);
  assert.equal(ob.bids[0].active, 1);
  assert.equal(ob.bids[0].price, 1_050_000n);
  assert.equal(ob.bids[0].size, 1_000_000n);
  assert.equal(ob.bids.length, 64);
  assert.equal(ob.bids[1].active, 0);
  assert.equal(ob.asks.length, 64);
  assert.equal(ob.events.length, 128);
  assert.equal(ob.observations.length, 16);
  assert.equal(ob.observations[0].slot, 100n);
  assert.equal(ob.observations[0].cumulativeMid, 2_000_000n);
  // mid helper over the decoded book.
  assert.equal(mid(ob.bestBid, ob.bestAsk), (1_050_000n + 1_100_000n) / 2n);
});

test("decoders return null for truncated buffers", () => {
  assert.equal(decodePerpMarket(Buffer.alloc(D + 100)), null);
  assert.equal(decodePosition(Buffer.alloc(D + 100)), null);
  assert.equal(decodeUserCollateral(Buffer.alloc(D + 10)), null);
  assert.equal(decodeYieldOracle(Buffer.alloc(D + 50)), null);
  assert.equal(decodeOrderBook(Buffer.alloc(D + 100)), null);
  assert.equal(decodePerpMarket(null), null);
});
