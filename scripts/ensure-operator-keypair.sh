#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 <operator-keypair-path>" >&2
  exit 1
fi

operator_keypair="$1"

if [[ -f "$operator_keypair" ]]; then
  echo "Operator keypair already exists at $operator_keypair"
  exit 0
fi

mkdir -p "$(dirname "$operator_keypair")"
solana-keygen new -o "$operator_keypair" -s --no-bip39-passphrase
echo "Operator keypair generated at $operator_keypair"
