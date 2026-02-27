#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "Usage: $0 <env-file> <key> <value>" >&2
  exit 1
fi

env_file="$1"
key="$2"
value="$3"
line="${key}=${value}"

dirname="$(dirname "$env_file")"
if [[ "$dirname" != "." ]]; then
  mkdir -p "$dirname"
fi

if [[ ! -f "$env_file" ]]; then
  printf '%s\n' "$line" > "$env_file"
  exit 0
fi

if grep -q "^${key}=" "$env_file"; then
  tmp_file="$(mktemp)"
  awk -v key="$key" -v value="$value" '
    $0 ~ "^" key "=" {
      print key "=" value
      next
    }
    { print }
  ' "$env_file" > "$tmp_file"
  mv "$tmp_file" "$env_file"
else
  printf '%s\n' "$line" >> "$env_file"
fi
