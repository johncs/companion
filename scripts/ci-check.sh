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
(( failed == 0 )) || exit 1
echo "  Line-count check passed."

echo "==> Checking zero unsafe blocks..."
if grep -rnE '(^|[^a-zA-Z0-9_])unsafe([^a-zA-Z0-9_]|$)' crates/; then
    echo "  [FAIL] Found unsafe block(s) in crates/"
    exit 1
fi
echo "  Zero unsafe blocks check passed."

echo "==> Checking non-test function length (<= 50 lines) and zero non-test #[allow(...)]..."
python3 - <<'PY'
import glob, re, sys

allows = []
long_fns = []

def strip_comments_and_strings(src):
    out = []
    i = 0
    n = len(src)
    while i < n:
        c = src[i]
        if src[i:i+2] == '//':
            while i < n and src[i] != '\n':
                out.append(' ')
                i += 1
        elif src[i:i+2] == '/*':
            out.append('  ')
            i += 2
            depth = 1
            while i < n and depth > 0:
                if src[i:i+2] == '/*':
                    depth += 1
                    out.append('  ')
                    i += 2
                elif src[i:i+2] == '*/':
                    depth -= 1
                    out.append('  ')
                    i += 2
                else:
                    out.append('\n' if src[i] == '\n' else ' ')
                    i += 1
        elif c == '"':
            out.append(' ')
            i += 1
            while i < n:
                if src[i] == '\\':
                    out.append('  ')
                    i += 2
                elif src[i] == '"':
                    out.append(' ')
                    i += 1
                    break
                else:
                    out.append('\n' if src[i] == '\n' else ' ')
                    i += 1
        elif src[i:i+2] == 'r#' or (c == 'r' and i + 1 < n and src[i+1] in '#"'):
            hashes = 0
            j = i + 1
            while j < n and src[j] == '#':
                hashes += 1
                j += 1
            if j < n and src[j] == '"':
                out.extend([' '] * (j - i + 1))
                i = j + 1
                end_seq = '"' + ('#' * hashes)
                while i < n:
                    if src[i:i+len(end_seq)] == end_seq:
                        out.extend([' '] * len(end_seq))
                        i += len(end_seq)
                        break
                    out.append('\n' if src[i] == '\n' else ' ')
                    i += 1
            else:
                out.append(c)
                i += 1
        elif c == "'":
            if i + 2 < n and src[i+1] != '\\' and src[i+2] == "'":
                out.append('   ')
                i += 3
            elif i + 3 < n and src[i+1] == '\\' and src[i+3] == "'":
                out.append('    ')
                i += 4
            else:
                out.append(c)
                i += 1
        else:
            out.append(c)
            i += 1
    return ''.join(out)

for path in sorted(glob.glob('crates/**/*.rs', recursive=True)):
    base = path.rsplit('/', 1)[-1]
    if base == 'tests.rs' or base.startswith('tests_') or base.endswith('_tests.rs') or '/tests/' in path:
        continue
    with open(path) as f:
        raw = f.read()
    clean = strip_comments_and_strings(raw)
    test_mod = re.search(r'#\[\s*cfg\s*\(\s*test\s*\)\s*\]\s*mod\s+tests\b', clean)
    clean_nontest = clean[:test_mod.start()] if test_mod else clean
    for m in re.finditer(r'#\[\s*allow\s*\(', clean_nontest):
        allows.append((path, clean_nontest[:m.start()].count('\n') + 1))
    for m in re.finditer(r'\b(?:pub(?:\s*\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?(?:extern\s+(?:"[^"]*"\s+)?)?fn\s+([A-Za-z_][A-Za-z0-9_]*)', clean_nontest):
        fn_name = m.group(1)
        start_line = clean_nontest[:m.start()].count('\n') + 1
        prefix = clean_nontest[max(0, m.start()-120):m.start()]
        if '#[test]' in prefix or '#[tokio::test]' in prefix:
            continue
        i = m.end()
        paren = 0
        brace = 0
        in_body = False
        body_end = None
        while i < len(clean_nontest):
            ch = clean_nontest[i]
            if not in_body:
                if ch == '(':
                    paren += 1
                elif ch == ')':
                    paren -= 1
                elif ch == ';' and paren == 0:
                    break
                elif ch == '{' and paren == 0:
                    in_body = True
                    brace = 1
            else:
                if ch == '{':
                    brace += 1
                elif ch == '}':
                    brace -= 1
                    if brace == 0:
                        body_end = i
                        break
            i += 1
        if body_end is not None:
            end_line = clean_nontest[:body_end].count('\n') + 1
            fn_len = end_line - start_line + 1
            if fn_len > 50:
                long_fns.append((path, start_line, fn_name, fn_len))

if allows:
    for p, ln in allows:
        print(f"  [FAIL] Non-test #[allow(...)] at {p}:{ln}")
if long_fns:
    for p, ln, name, n in long_fns:
        print(f"  [FAIL] {p}:{ln} fn {name} is {n} lines (> 50)")
if allows or long_fns:
    sys.exit(1)
print("  Zero #[allow(...)] and <= 50-line function checks passed.")
PY

echo "==> Checking full-workspace po/POTFILES.in sync..."
python3 - <<'PY'
import glob, os, re, sys

with open('po/POTFILES.in') as f:
    pot_entries = [l.strip() for l in f if l.strip() and not l.strip().startswith('#')]

pot_set = set(pot_entries)
failed = False

for entry in pot_entries:
    if not os.path.isfile(entry):
        print(f"  [FAIL] Missing file in po/POTFILES.in: {entry}")
        failed = True
    elif entry.endswith('.rs'):
        with open(entry) as f:
            raw = f.read()
        if not re.search(r'\b(gettext|ngettext|tr_f)\(', raw):
            print(f"  [FAIL] Stale entry in po/POTFILES.in (no gettext/ngettext/tr_f): {entry}")
            failed = True

for p in sorted(glob.glob('crates/**/*.rs', recursive=True)):
    if p == 'crates/companion-core/src/i18n.rs':
        continue
    with open(p) as f:
        raw = f.read()
    if re.search(r'\b(gettext|ngettext|tr_f)\(', raw) and p not in pot_set:
        print(f"  [FAIL] {p} uses gettext/ngettext/tr_f but is missing from po/POTFILES.in")
        failed = True

if failed:
    sys.exit(1)
print("  po/POTFILES.in sync check passed.")
PY

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
(( sleep_failed == 0 )) || exit 1
echo "  Zero test-sleep check passed."

echo "==> Checking zero std::fs calls in crates/companion-app/src/ui/..."
fs_failed=0
while IFS= read -r -d '' file; do
    matches=$(awk '
        /#\[cfg\(test\)\]/ { in_test = 1 }
        !in_test && /std::fs/ { print FILENAME ":" NR ":" $0 }
    ' "$file")
    if [[ -n "$matches" ]]; then
        echo "$matches"
        echo "  [FAIL] Found std::fs outside #[cfg(test)] in UI file: $file"
        fs_failed=1
    fi
done < <(find crates/companion-app/src/ui -name '*.rs' -print0 | sort -z)
(( fs_failed == 0 )) || exit 1
echo "  Zero UI std::fs check passed."

echo "==> Checking desktop and AppStream metadata (if tools installed)..."
if command -v desktop-file-validate >/dev/null 2>&1; then
    tmp_desktop=$(mktemp --suffix=.desktop)
    sed 's/@app_id@/io.github.companion.Companion/g' data/io.github.companion.Companion.desktop.in > "$tmp_desktop"
    desktop-file-validate "$tmp_desktop"
    rm -f "$tmp_desktop"
fi
if command -v appstreamcli >/dev/null 2>&1; then
    tmp_meta=$(mktemp --suffix=.metainfo.xml)
    sed 's/@app_id@/io.github.companion.Companion/g' data/io.github.companion.Companion.metainfo.xml.in > "$tmp_meta"
    appstreamcli validate --no-net --explain "$tmp_meta"
    rm -f "$tmp_meta"
fi
echo "  All CI checks passed."
