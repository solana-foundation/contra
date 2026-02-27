#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "Usage: $0 <env-file> <env-key> <program-keypair-path>" >&2
  exit 1
fi

env_file="$1"
env_key="$2"
program_keypair="$3"
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

program_id="$(solana-keygen pubkey "$program_keypair")"
"$script_dir/upsert-env.sh" "$env_file" "$env_key" "$program_id"

echo "Updated $env_file with $env_key=$program_id"
