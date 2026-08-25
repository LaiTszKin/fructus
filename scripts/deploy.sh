#!/usr/bin/env bash
# scripts/deploy.sh — build + deploy the Fructus program to devnet (R-D1b).
#
# Does NOT modify the localnet default profile. Deploys to the devnet cluster by
# selecting the devnet provider per-command (`--provider.cluster devnet
# --provider.wallet <path>`), which Anchor supports while a single `[provider]`
# table stays tuned for localnet.
#
# PREREQUISITES
#   * `anchor` + `cargo-build-sbf` on PATH.
#   * A devnet-funded keypair (not committed). Point DEVNET_WALLET at it.
#   * `npm install` in scripts/ (for @solana/web3.js, used only to derive PDAs).
#
# ENV
#   DEVNET_WALLET   (required) path to the devnet deploy keypair (.json)
#   DEVNET_CLUSTER  (optional) cluster name, default "devnet"
#   PROGRAM_ID      (optional) override the devnet program id; else read from
#                                  Anchor.toml [programs.devnet]
#
# After a successful deploy the script DERIVES the PerpMarket + OrderBook PDAs
# (seed "perp_market" / ["order_book", market]) and records everything to
# scripts/deploy-output.json.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

: "${DEVNET_WALLET:?DEVNET_WALLET is required: path to the devnet deploy keypair}"
DEVNET_CLUSTER="${DEVNET_CLUSTER:-devnet}"

# The Anchor.toml [programs.devnet] id ships as a valid-base58 PLACEHOLDER so
# `anchor build` keeps working; it must be replaced with the real devnet id.
DEVNET_PROGRAM_ID_PLACEHOLDER="J2xccRtuG43drESLYznHhLhQkLTdfepcKYbiQ9BsJVaf"

# Resolve the devnet program id: explicit env wins, else Anchor.toml [programs.devnet].
program_id=""
if [[ -n "${PROGRAM_ID:-}" ]]; then
  program_id="$PROGRAM_ID"
else
  # The anchor-toml devnet id is on the first `fructus = "..."`
  # line after the [programs.devnet] header.
  program_id="$(awk '
    /^\[programs\.devnet\]/ {in_devnet=1; next}
    /^\[/ {in_devnet=0}
    in_devnet && /^[[:space:]]*fructus[[:space:]]*=/ {
      sub(/^[[:space:]]*fructus[[:space:]]*=[[:space:]]*/, "", $0);
      gsub(/[", ]/, "", $0);
      print $0
    }
  ' Anchor.toml)"
fi

if [[ -z "$program_id" || "$program_id" == "$DEVNET_PROGRAM_ID_PLACEHOLDER" ]]; then
  echo "error: the devnet program id is unset (or still the placeholder)." >&2
  echo "  Set PROGRAM_ID=<id> or fill [programs.devnet] fructus in Anchor.toml," >&2
  echo "  and make sure it matches declare_id! in programs/fructus/src/lib.rs." >&2
  exit 1
fi

echo "==> [1/4] anchor build (devnet program id: $program_id)"
anchor build

echo "==> [2/4] anchor deploy to cluster '$DEVNET_CLUSTER'"
anchor deploy \
  --program-name fructus \
  --provider.cluster "$DEVNET_CLUSTER" \
  --provider.wallet "$DEVNET_WALLET"

echo "==> [3/4] deriving PDAs from program id: $program_id"
# @solana/web3.js is resolved from scripts/node_modules (`npm install`).
if [[ ! -d scripts/node_modules ]]; then
  echo "warn: scripts/node_modules missing; installing deps to derive PDAs"
  (cd scripts && npm install --silent)
fi
out_dir="$(cd scripts && pwd)"
out_json="$(node -e '
  const { PublicKey } = require("@solana/web3.js");
  const id = new PublicKey(process.argv[1]);
  const [market, mb] = PublicKey.findProgramAddressSync([Buffer.from("perp_market")], id);
  const [book, bb] = PublicKey.findProgramAddressSync([Buffer.from("order_book"), market.toBytes()], id);
  const [vault, vb] = PublicKey.findProgramAddressSync([Buffer.from("vault")], id);
  process.stdout.write(JSON.stringify({
    program_id: id.toString(),
    market: market.toString(),
    market_bump: mb,
    order_book: book.toString(),
    order_book_bump: bb,
    vault: vault.toString(),
    vault_bump: vb,
  }, null, 2));
' "$program_id")"

echo "==> [4/4] recording deployment to scripts/deploy-output.json"
printf '%s\n' "$out_json" > "$out_dir/deploy-output.json"
echo "$out_json"

echo
echo "Recording the deployed id back into Anchor.toml (programs.devnet):"
echo "  sed -i \"s#^fructus = .*#fructus = \\\"$program_id\\\"#\" Anchor.toml"
echo "If declare_id! differs, update it too, rebuild, and re-run this script."
echo "Then set these env vars and run the e2e:"
echo "  PROGRAM_ID=$program_id MARKET_ADDRESS=$(node -e 'process.stdout.write(require(process.argv[1]).market)' "$out_dir/deploy-output.json")"
