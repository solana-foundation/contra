#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "Usage: $0 <env-file> <operator-keypair-path>" >&2
  exit 1
fi

env_file="$1"
operator_keypair="$2"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

operator_pubkey="$(solana-keygen pubkey "$operator_keypair")"
operator_private_key="$(tr -d '\n' < "$operator_keypair")"

"$script_dir/upsert-env.sh" "$env_file" "PRIVATE_CHANNEL_ADMIN_KEYS" "$operator_pubkey"
"$script_dir/upsert-env.sh" "$env_file" "ADMIN_PRIVATE_KEY" "$operator_private_key"

echo "Updated $env_file with PRIVATE_CHANNEL_ADMIN_KEYS=$operator_pubkey"
echo "Updated $env_file with ADMIN_PRIVATE_KEY"
