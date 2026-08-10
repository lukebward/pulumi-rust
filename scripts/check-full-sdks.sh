#!/usr/bin/env bash
#
# Generate and compile the *whole* SDK for every provider the examples pin,
# then compile every example that pins it against that whole crate.
#
# The per-example check under examples/ generates a subset of each provider's
# schema — only the members that example touches — because the full crates are
# tens of megabytes of Rust apiece. A subset cannot surface a defect that two
# members produce only together: two schema tokens deriving the same Rust type
# name are invisible unless both are generated at once. This script is the
# other half of the check.
#
# Needs `pulumi` and `cargo` on PATH, and network access for both the provider
# plugins and crates.io.
#
#   scripts/check-full-sdks.sh              # every provider the examples pin
#   scripts/check-full-sdks.sh aws@7.41.0   # just one
#
set -uo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
work=${WORK_DIR:-$(mktemp -d)}
: "${CARGO_TARGET_DIR:=$work/target}"
export CARGO_TARGET_DIR

if [ $# -gt 0 ]; then
    specs=("$@")
else
    # The versions the examples pin, deduplicated, straight from the
    # `gen-sdk` line each example's Cargo.toml carries — so this list cannot
    # drift from what the examples are actually checked against.
    mapfile -t specs < <(
        grep -rhoP --include=Cargo.toml \
            '(?<=pulumi package gen-sdk )[a-z-]+@[0-9][0-9a-zA-Z.+-]*' \
            "$root/examples" | sort -u
    )
fi

echo "checking ${#specs[@]} provider SDKs in $work"
failed=()
for spec in "${specs[@]}"; do
    name=${spec%@*}
    out=$work/$name
    rm -rf "$out"

    if ! pulumi package gen-sdk "$spec" --language rust --out "$out" >"$work/$name.gen.log" 2>&1; then
        echo "FAIL $spec: gen-sdk"
        tail -20 "$work/$name.gen.log"
        failed+=("$spec")
        continue
    fi

    # The generated crate declares `pulumi = "0.1"`, which is not published.
    sed -i.bak "s|^pulumi = \"0.1\"$|pulumi = { path = \"$root/sdk/rust/pulumi\" }|" \
        "$out/rust/Cargo.toml"
    rm -f "$out/rust/Cargo.toml.bak"

    if (cd "$out/rust" && cargo check) >"$work/$name.check.log" 2>&1; then
        structs=$(grep -c '^\s*pub struct ' "$out/rust/src/lib.rs")
        size=$(du -h "$out/rust/src/lib.rs" | cut -f1)
        echo "ok   $spec ($structs structs, $size)"
    else
        echo "FAIL $spec: cargo check"
        grep -E '^(error|warning: unused)' "$work/$name.check.log" | sort | uniq -c | head -20
        failed+=("$spec")
        continue
    fi

    # Every example that pins this provider, compiled against the crate just
    # generated. Each example is normally checked against a subset of its
    # provider's schema; this is the same programs against the whole thing.
    # Built out-of-tree so the repository keeps no generated SDK and no
    # rewritten Cargo.toml.
    for ex in "$root"/examples/*/; do
        exname=$(basename "$ex")
        grep -q "\"\./sdks/$name/rust\"" "$ex/Cargo.toml" 2>/dev/null || continue

        rm -rf "$work/ex-$exname"
        cp -r "$ex" "$work/ex-$exname"
        rm -rf "$work/ex-$exname/sdks" "$work/ex-$exname/target"
        sed -i.bak \
            -e "s|path = \"../../sdk/rust/pulumi\"|path = \"$root/sdk/rust/pulumi\"|" \
            -e "s|path = \"./sdks/$name/rust\"|path = \"$out/rust\"|" \
            "$work/ex-$exname/Cargo.toml"
        rm -f "$work/ex-$exname/Cargo.toml.bak"

        if (cd "$work/ex-$exname" && cargo check) >"$work/ex-$exname.log" 2>&1; then
            echo "  ok   $exname"
        else
            echo "  FAIL $exname"
            grep -E '^error' "$work/ex-$exname.log" | head -8
            failed+=("$exname")
        fi
    done
done

if [ ${#failed[@]} -gt 0 ]; then
    echo
    echo "${#failed[@]} failed: ${failed[*]}"
    echo "logs in $work"
    exit 1
fi
echo "all ${#specs[@]} compile"
