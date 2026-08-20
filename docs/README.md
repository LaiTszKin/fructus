# Fructus Documentation

Fructus is a Solana protocol for trading yield futures — turning the variable
yield of liquid-staking assets (starting with jitoSOL) into a tradeable
instrument. This tree documents the **data module** (the mark-price APY oracle,
trustless settlement reference, off-chain keeper, and fuzz harness) and the
**perpetual-market** account.

## Quick Links

- [Setup](setup.md) · [Architecture](architecture.md) · [API Reference](api-reference.md)
- Back to project [README](../README.md)

## I want to…

| I want to… | Go to |
| --- | --- |
| Build / run / install | [setup.md](setup.md) |
| Understand the system design | [architecture.md](architecture.md) |
| See the on-chain instruction surface | [api-reference.md](api-reference.md) |
| See account/field layouts | [data-models.md](data-models.md) |
| Understand the mark-price oracle | [modules/oracle.md](modules/oracle.md) |
| Understand trustless settlement | [modules/settlement.md](modules/settlement.md) |
| Understand the perpetual market | [modules/market.md](modules/market.md) |
| Understand the on-chain order book | [modules/order-book.md](modules/order-book.md) |
| Understand the collateral vault | [modules/collateral.md](modules/collateral.md) |
| Understand the off-chain keeper | [modules/publisher.md](modules/publisher.md) |
| Run tests / fuzz | [testing.md](testing.md) |
| Learn code conventions | [conventions.md](conventions.md) |
| Follow a task recipe | [workflows.md](workflows.md) |

## Document Index

- [tech-stack.md](tech-stack.md) — languages, frameworks, versions
- [project-structure.md](project-structure.md) — directory map
- [architecture.md](architecture.md) — C4 diagrams + decisions
- [conventions.md](conventions.md) — code rules that differ from defaults
- [modules/oracle.md](modules/oracle.md) — mark-price APY oracle
- [modules/settlement.md](modules/settlement.md) — trustless exchange rate / yield
- [modules/market.md](modules/market.md) — perpetual market + `initialize_market`
- [modules/order-book.md](modules/order-book.md) — order book + matching engine + mark/twap
- [modules/collateral.md](modules/collateral.md) — collateral vault + deposit/withdraw
- [modules/publisher.md](modules/publisher.md) — off-chain keeper
- [api-reference.md](api-reference.md) — instruction reference + signature scheme
- [data-models.md](data-models.md) — account + derived-rate layouts
- [setup.md](setup.md) — getting started
- [testing.md](testing.md) — test strategy + commands
- [workflows.md](workflows.md) — task recipes
