#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

version_file="$(tr -d '[:space:]' < VERSION)"
cargo_version="$(cargo metadata --no-deps --format-version 1 \
  | jq -r '.packages[] | select(.name == "grow-core") | .version')"

if [[ "$version_file" != "$cargo_version" ]]; then
  echo "version mismatch: VERSION=$version_file workspace=$cargo_version" >&2
  exit 1
fi

bad_lock_entries="$(cargo metadata --no-deps --format-version 1 \
  | jq -r --arg version "$version_file" '
      .packages[]
      | select(.name | startswith("grow-"))
      | select(.version != $version)
      | "\(.name)=\(.version)"
    ')"

if [[ -n "$bad_lock_entries" ]]; then
  echo "workspace package versions differ from VERSION:" >&2
  echo "$bad_lock_entries" >&2
  exit 1
fi

echo "versions aligned: $version_file"
