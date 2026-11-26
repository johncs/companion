#!/usr/bin/env bash
set -euo pipefail

echo "==> Checking formatting..."
cargo fmt --all -- --check

echo "==> Running clippy..."
cargo clippy --workspace --all-targets -- -D warnings

echo "==> Running workspace tests..."
cargo test --workspace

echo "==> Checking Rust file line counts (<= 400 lines)..."
failed=0
while IFS= read -r -d '' file; do
    lines=$(wc -l < "$file")
    if (( lines > 400 )); then
        echo "  [FAIL] $file has $lines lines (> 400)"
        failed=1
    fi
done < <(find crates -name '*.rs' -print0 | sort -z)

if (( failed != 0 )); then
    exit 1
fi
echo "  Line-count check passed."

echo "==> Checking zero unsafe blocks..."
if grep -rnE '(^|[^a-zA-Z0-9_])unsafe([^a-zA-Z0-9_]|$)' crates/; then
    echo "  [FAIL] Found unsafe block(s) in crates/"
    exit 1
fi
echo "  Zero unsafe blocks check passed."

echo "==> Checking po/POTFILES.in drift..."
pot_failed=0
while IFS= read -r entry; do
    [[ -z "$entry" || "$entry" == \#* ]] && continue
    if [[ ! -f "$entry" ]]; then
        echo "  [FAIL] Missing file in po/POTFILES.in: $entry"
        pot_failed=1
    fi
done < po/POTFILES.in

while IFS= read -r -d '' ui_file; do
    if grep -qE '\b(gettext|tr_f)\(' "$ui_file"; then
        if ! grep -qxF "$ui_file" po/POTFILES.in; then
            echo "  [FAIL] $ui_file uses gettext/tr_f but is missing from po/POTFILES.in"
            pot_failed=1
        fi
    fi
done < <(find crates/companion-app/src -name '*.rs' -print0 | sort -z)

if (( pot_failed != 0 )); then
    exit 1
fi
echo "  po/POTFILES.in check passed."

echo "==> Checking zero self-referential callback webs..."
if grep -rnE 'Rc<RefCell<Option<Rc<dyn Fn' crates/; then
    echo "  [FAIL] Found Rc<RefCell<Option<Rc<dyn Fn... callback web in crates/"
    exit 1
fi
echo "  Callback-web check passed."

echo "==> Checking zero sleep calls in tests..."
sleep_failed=0
while IFS= read -r -d '' file; do
    base=$(basename "$file")
    if [[ "$file" == */tests/* || "$base" == tests.rs || "$base" == *_tests.rs || "$base" == tests_*.rs ]]; then
        if grep -nE '(thread::sleep|time::sleep)\(' "$file"; then
            echo "  [FAIL] Found sleep() in test file: $file"
            sleep_failed=1
        fi
    else
        matches=$(awk '
            /#\[cfg\(test\)\]/ { in_test = 1 }
            in_test && /(thread::sleep|time::sleep)\(/ { print FILENAME ":" NR ":" $0 }
        ' "$file")
        if [[ -n "$matches" ]]; then
            echo "$matches"
            echo "  [FAIL] Found sleep() inside #[cfg(test)] in: $file"
            sleep_failed=1
        fi
    fi
done < <(find crates -name '*.rs' -print0 | sort -z)

if (( sleep_failed != 0 )); then
    exit 1
fi
echo "  Zero test-sleep check passed."
