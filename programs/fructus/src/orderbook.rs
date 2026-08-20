//! Pure on-chain order-book matching engine + mark/twap primitives.
//!
//! This module is deliberately free of Anchor account plumbing (mirroring the
//! `exchange.rs` split): [`Book`] is a plain in-memory bid/ask model that
//! `proptest` drives directly, and the thin `place_limit_order` /
//! `place_market_order` / `cancel_order` / `crank` adapters in `lib.rs` load and
//! save the on-chain `OrderBook` account into/from it.
//!
//! Representation (design doc §5):
//! * `price` is the traded yield level in `APY_SCALE` (`1_000_000`) fixed point,
//!   tick size `1`. Zero is **not** a valid resting price.
//! * `size` is notional USDC microunits (6 decimals) — the matching invariants
//!   are unit-agnostic.
//! * `seq` is the monotonically increasing order id giving time priority.
//!
//! All arithmetic is `u64`/`u128` with `checked_*`/`saturating_*` — there is no
//! panicking math and no indexing that can panic on malformed input. No new
//! dependency is introduced.

use anchor_lang::prelude::*;

use crate::constants::MAX_ORDERS_PER_SIDE;
use crate::error::FructusError;

/// Which side of the book an order rests on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Bid,
    Ask,
}

/// How an incoming order executes: rest (limit) or cross (market, IOC).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderKind {
    Limit,
    Market,
}

/// A resting order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Order {
    pub owner: Pubkey,
    pub side: Side,
    pub price: u64,
    pub size: u64,
    pub seq: u64,
}

/// One fill produced by the engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fill {
    pub maker_seq: u64,
    pub maker_owner: Pubkey,
    pub taker_owner: Pubkey,
    pub size: u64,
    pub price: u64,
}

/// The engine's result: the fills and any unfilled remainder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchOutcome {
    pub fills: Vec<Fill>,
    pub residual: Option<Order>,
}

/// In-memory order book (pure model).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Book {
    pub bids: Vec<Order>,
    pub asks: Vec<Order>,
    pub next_seq: u64,
}

/// A time-weighted-mid accumulator sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observation {
    pub slot: u64,
    pub cumulative_mid: u128,
}

/// Whether a bid at `bid` crosses an ask at `ask`: equal prices cross.
pub fn is_crossable(bid: u64, ask: u64) -> bool {
    bid >= ask
}

/// Whether `cand` is a strictly better resting price than `best` for `side`.
///
/// Bids improve by rising; asks improve by falling.
pub fn price_better(cand: u64, best: u64, side: Side) -> bool {
    match side {
        Side::Bid => cand > best,
        Side::Ask => cand < best,
    }
}

/// Best (highest) resting bid price, or `0` when the bid side is empty.
pub fn best_bid(book: &Book) -> u64 {
    book.bids.iter().map(|o| o.price).max().unwrap_or(0)
}

/// Best (lowest) resting ask price, or `0` when the ask side is empty.
pub fn best_ask(book: &Book) -> u64 {
    book.asks.iter().map(|o| o.price).min().unwrap_or(0)
}

/// Whether `order` (a limit order) would cross the opposite side of `book`.
///
/// A bid crosses when it is at or above the best ask; an ask crosses when it is
/// at or below the best bid. An empty opposite side never crosses.
pub fn would_cross(book: &Book, order: &Order) -> bool {
    match order.side {
        Side::Bid => {
            let ask = best_ask(book);
            ask != 0 && is_crossable(order.price, ask)
        }
        Side::Ask => {
            let bid = best_bid(book);
            bid != 0 && is_crossable(bid, order.price)
        }
    }
}

/// Mid price: `(best_bid + best_ask) / 2`, truncating toward zero.
///
/// Returns `None` iff either side is empty (`best_bid == 0 || best_ask == 0`).
/// The sum uses `u128` so `best_bid + best_ask` cannot overflow, and the result
/// is always within `[best_bid, best_ask]` when `Some`.
pub fn mid(book: &Book) -> Option<u64> {
    let bid = best_bid(book);
    let ask = best_ask(book);
    if bid == 0 || ask == 0 {
        return None;
    }
    let sum = (bid as u128) + (ask as u128);
    u64::try_from(sum / 2).ok()
}

/// Run the matching engine against the opposite side of `book`.
///
/// * **Price-time priority** — best price first, then lowest `seq`.
/// * **Partial fills** — each fill is `min(taker remaining, maker remaining)`;
///   fully-filled makers are removed, the rest keep their reduced size.
/// * **No over-fill** — `Σ fills.size <= incoming.size`, each fill `<=` its
///   maker's pre-fill size.
/// * **No self-trade** — a maker owned by `incoming.owner` is skipped.
/// * **Bounded** — stops after `max_steps` fills, leaving any still-crossable
///   remainder as `residual` (the crank re-matches it later); state is never
///   left corrupt.
///
/// [`OrderKind::Market`] is immediate-or-cancel: an unfilled remainder is
/// cancelled, so `residual` is always `None`.
pub fn match_order(
    book: &mut Book,
    incoming: Order,
    kind: OrderKind,
    max_steps: u64,
) -> MatchOutcome {
    let mut remaining = incoming.size;
    let mut fills: Vec<Fill> = Vec::new();
    let mut steps: u64 = 0;
    let mut budget_hit = false;

    loop {
        if remaining == 0 {
            break;
        }
        let Some(idx) = best_crossable_maker(book, &incoming, kind) else {
            break;
        };
        if steps >= max_steps {
            // Compute budget exhausted with a crossable maker still available:
            // the remainder is deferred (re-queued) rather than dropped.
            budget_hit = true;
            break;
        }

        let (fill_size, fully_filled) = {
            let maker = match incoming.side {
                Side::Bid => &mut book.asks[idx],
                Side::Ask => &mut book.bids[idx],
            };
            let fill_size = remaining.min(maker.size);
            fills.push(Fill {
                maker_seq: maker.seq,
                maker_owner: maker.owner,
                taker_owner: incoming.owner,
                size: fill_size,
                price: maker.price,
            });
            maker.size -= fill_size;
            (fill_size, maker.size == 0)
        };
        remaining -= fill_size;
        steps += 1;
        if fully_filled {
            match incoming.side {
                Side::Bid => {
                    book.asks.remove(idx);
                }
                Side::Ask => {
                    book.bids.remove(idx);
                }
            }
        }
    }

    let residual = match kind {
        OrderKind::Market => None,
        OrderKind::Limit if budget_hit => Some(Order {
            owner: incoming.owner,
            side: incoming.side,
            price: incoming.price,
            size: remaining,
            seq: incoming.seq,
        }),
        OrderKind::Limit => None,
    };

    MatchOutcome { fills, residual }
}

/// Rest a limit order, but only when it does **not** cross the opposite side.
///
/// A crossing order is rejected (it must be matched by [`match_order`], never
/// rested against the opposite side), and a zero-price order is rejected with
/// [`FructusError::InvalidPrice`]. When the order's side is already at
/// [`MAX_ORDERS_PER_SIDE`] capacity the call fails with
/// [`FructusError::BookFull`] and the book is left unchanged.
pub fn post_limit(book: &mut Book, order: Order) -> Result<()> {
    // A resting order must carry a valid (non-zero) price.
    require!(order.price != 0, FructusError::InvalidPrice);

    // Never rest a crossing pair: a crossing limit must be matched by the
    // caller, not left resting against the opposite side (design D5).
    if would_cross(book, &order) {
        return Err(FructusError::InvalidPrice.into());
    }

    match order.side {
        Side::Bid => {
            require!(
                book.bids.len() < MAX_ORDERS_PER_SIDE,
                FructusError::BookFull
            );
            book.bids.push(order);
        }
        Side::Ask => {
            require!(
                book.asks.len() < MAX_ORDERS_PER_SIDE,
                FructusError::BookFull
            );
            book.asks.push(order);
        }
    }
    Ok(())
}

/// Cancel one resting order, by owner and sequence id.
///
/// Removes exactly one order on either side and returns it; a non-owner fails
/// with [`FructusError::OrderOwnerMismatch`] and an absent/unknown id fails with
/// [`FructusError::OrderNotFound`], neither of which mutates the book.
pub fn cancel(book: &mut Book, owner: Pubkey, seq: u64) -> Result<Order> {
    if let Some(pos) = book.bids.iter().position(|o| o.seq == seq) {
        require!(
            book.bids[pos].owner == owner,
            FructusError::OrderOwnerMismatch
        );
        return Ok(book.bids.remove(pos));
    }
    if let Some(pos) = book.asks.iter().position(|o| o.seq == seq) {
        require!(
            book.asks[pos].owner == owner,
            FructusError::OrderOwnerMismatch
        );
        return Ok(book.asks.remove(pos));
    }
    Err(FructusError::OrderNotFound.into())
}

/// Time-weighted average mid over `window_slots` ending at `now_slot`.
///
/// Computes `(cum_now - cum_then) / window_slots` from the running
/// `cumulative_mid` accumulator, where `cum_then` is the value at
/// `now_slot - window_slots`. Returns `None` for a zero window, an empty
/// history, a history that does not reach back a full window, or any
/// `u128 -> u64` overflow. Purely deterministic; no panicking math.
pub fn twap(obs: &[Observation], window_slots: u64, now_slot: u64) -> Option<u64> {
    if window_slots == 0 || obs.is_empty() {
        return None;
    }
    let start_slot = now_slot.checked_sub(window_slots)?;
    let cum_now = cumulative_at(obs, now_slot)?;
    let cum_start = cumulative_at(obs, start_slot)?;
    let delta = cum_now.checked_sub(cum_start)?;
    let avg = delta / (window_slots as u128);
    u64::try_from(avg).ok()
}

/// The best crossable maker for `taker` on the opposite side, skipping
/// self-owned makers. Price-time priority, so an `Option<usize>` index into the
/// opposite side's `Vec` is returned rather than the order itself.
fn best_crossable_maker(book: &Book, taker: &Order, kind: OrderKind) -> Option<usize> {
    let (resting, resting_side) = match taker.side {
        Side::Bid => (book.asks.as_slice(), Side::Ask),
        Side::Ask => (book.bids.as_slice(), Side::Bid),
    };
    let mut best: Option<(usize, u64, u64)> = None;
    for (i, maker) in resting.iter().enumerate() {
        if maker.owner == taker.owner {
            continue; // no self-trade
        }
        let crossable = match kind {
            OrderKind::Market => true,
            OrderKind::Limit => match taker.side {
                Side::Bid => is_crossable(taker.price, maker.price),
                Side::Ask => is_crossable(maker.price, taker.price),
            },
        };
        if !crossable {
            continue;
        }
        let better = match best {
            None => true,
            Some((_, best_price, best_seq)) => {
                price_better(maker.price, best_price, resting_side)
                    || (maker.price == best_price && maker.seq < best_seq)
            }
        };
        if better {
            best = Some((i, maker.price, maker.seq));
        }
    }
    best.map(|(i, _, _)| i)
}

/// The running `cumulative_mid` at `slot`, interpolated piecewise-linearly
/// between the surrounding observations.
///
/// The accumulator is piecewise-linear — `mid` is constant between consecutive
/// observations (see `record_observation`) — so an off-grid `Clock::get().slot`
/// yields the exact cumulative value rather than `None`. Returns `None` only
/// when `slot` precedes the first observation (the history does not reach back
/// that far) or when there is no trailing segment to extrapolate with.
fn cumulative_at(obs: &[Observation], slot: u64) -> Option<u128> {
    if obs.is_empty() {
        return None;
    }
    if let Some(o) = obs.iter().find(|o| o.slot == slot) {
        return Some(o.cumulative_mid);
    }
    let lo = obs
        .iter()
        .filter(|o| o.slot < slot)
        .max_by_key(|o| o.slot)?;
    if let Some(hi) = obs.iter().filter(|o| o.slot > slot).min_by_key(|o| o.slot) {
        interpolate(lo, hi, slot)
    } else {
        // `slot` is after the last observation: extrapolate with the trailing
        // mid (the slope of the last observed segment).
        let prev = obs
            .iter()
            .filter(|o| o.slot < lo.slot)
            .max_by_key(|o| o.slot)?;
        interpolate(prev, lo, slot)
    }
}

/// `cumulative_mid` at `slot`, linearly interpolated along the segment
/// `(lo.slot, hi.slot]`, whose mid is constant and equal to
/// `(hi.cumulative_mid - lo.cumulative_mid) / (hi.slot - lo.slot)`.
fn interpolate(lo: &Observation, hi: &Observation, slot: u64) -> Option<u128> {
    let span = hi.slot.checked_sub(lo.slot)?;
    if span == 0 {
        return None;
    }
    let offset = slot.checked_sub(lo.slot)?;
    let rise = hi.cumulative_mid.checked_sub(lo.cumulative_mid)?;
    let inc = rise
        .checked_mul(offset as u128)?
        .checked_div(span as u128)?;
    lo.cumulative_mid.checked_add(inc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::MAX_ORDERS_PER_SIDE;
    use anchor_lang::prelude::Pubkey;

    fn order(owner: u8, side: Side, price: u64, size: u64, seq: u64) -> Order {
        Order {
            owner: Pubkey::from([owner; 32]),
            side,
            price,
            size,
            seq,
        }
    }

    fn book(bids: Vec<Order>, asks: Vec<Order>) -> Book {
        Book {
            bids,
            asks,
            next_seq: 0,
        }
    }

    #[test]
    fn post_limit_rejects_zero_price() {
        let mut b = book(vec![], vec![]);
        assert!(post_limit(&mut b, order(1, Side::Bid, 0, 10, 0)).is_err());
        assert!(post_limit(&mut b, order(1, Side::Ask, 0, 10, 1)).is_err());
        assert!(b.bids.is_empty() && b.asks.is_empty());
    }

    #[test]
    fn post_limit_rejects_crossing() {
        let mut b = book(vec![], vec![order(2, Side::Ask, 10, 5, 0)]);
        assert!(post_limit(&mut b, order(1, Side::Bid, 12, 5, 1)).is_err());
        assert_eq!(b.asks.len(), 1);
        assert!(b.bids.is_empty());

        let mut b2 = book(vec![order(2, Side::Bid, 10, 5, 0)], vec![]);
        assert!(post_limit(&mut b2, order(1, Side::Ask, 9, 5, 1)).is_err());
        assert_eq!(b2.bids.len(), 1);
        assert!(b2.asks.is_empty());
    }

    #[test]
    fn post_limit_rests_non_crossing() {
        let mut b = book(vec![], vec![order(2, Side::Ask, 10, 5, 0)]);
        assert!(post_limit(&mut b, order(1, Side::Bid, 9, 5, 1)).is_ok());
        assert_eq!(best_bid(&b), 9);
        assert_eq!(best_ask(&b), 10);
        assert!(mid(&b).is_some());
    }

    #[test]
    fn post_limit_rejects_when_full() {
        let mut b = book(vec![], vec![]);
        for i in 0..MAX_ORDERS_PER_SIDE {
            assert!(post_limit(&mut b, order(1, Side::Bid, 1 + i as u64, 1, i as u64)).is_ok());
        }
        assert!(post_limit(
            &mut b,
            order(1, Side::Bid, 999, 1, MAX_ORDERS_PER_SIDE as u64)
        )
        .is_err());
        assert_eq!(b.bids.len(), MAX_ORDERS_PER_SIDE);
    }
}
