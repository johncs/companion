#!/usr/bin/env bash
set -euo pipefail

export PATH="/usr/bin:${PATH}"

echo "==> Checking formatting..."
cargo fmt --all -- --check

echo "==> Running clippy..."
cargo clippy --workspace --all-targets -- -D warnings

echo "==> Running workspace tests..."
cargo test --workspace

echo "==> Checking Rust file line counts (> 400 lines)..."
oversized=0
while IFS= read -r -d '' file; do
    lines=$(wc -l < "$file")
    if (( lines > 400 )); then
        echo "  [WARN] $file has $lines lines (> 400)"
        oversized=1
    fi
done < <(find crates -name '*.rs' -print0 | sort -z)

if (( oversized == 0 )); then
    echo "  All .rs files are <= 400 lines."
fi
