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
# azure-native is the one provider not generated from its plugin. The plugin
# serves a schema spanning every Azure API version — ~490 MB of JSON that
# generates ~440 MB of Rust, more than rustc can compile as one crate. The
# azure examples instead document generating from the default-version schema
# the provider checks into its repository, so that is the schema checked
# here. See examples/README.md.
#
# Needs `pulumi` and `cargo` on PATH, and network access for the provider
# plugins, the azure-native schema, and crates.io.
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
    # The versions the examples pin, deduplicated, straight out of each
    # example's Cargo.toml — the `gen-sdk` line for most providers, the
    # schema URL for azure-native — so this list cannot drift from what the
    # examples are actually checked against.
    # No mapfile and no grep -P: both are GNU-only, and on macOS the empty
    # list they produced made this script pass having checked nothing.
    specs=()
    while IFS= read -r spec; do
        specs+=("$spec")
    done < <(
        {
            grep -rhoE --include=Cargo.toml \
                'pulumi package gen-sdk [a-z-]+@[0-9][0-9a-zA-Z.+-]*' \
                "$root/examples" | awk '{print $NF}'
            grep -rhoE --include=Cargo.toml \
                'pulumi-azure-native/v[0-9][0-9a-zA-Z.+-]*/' \
                "$root/examples" | sed 's|pulumi-azure-native/v\(.*\)/|azure-native@\1|'
        } | sort -u
    )
    if [ ${#specs[@]} -eq 0 ]; then
        echo "error: found no gen-sdk pins under $root/examples" >&2
        exit 1
    fi
fi

echo "checking ${#specs[@]} provider SDKs in $work"
failed=()
for spec in "${specs[@]}"; do
    name=${spec%@*}
    version=${spec#*@}
    out=$work/$name
    rm -rf "$out"

    # azure-native generates from the checked-in default-version schema, as
    # its examples document; every other provider generates from its plugin.
    src=$spec
    if [ "$name" = azure-native ]; then
        src=$work/azure-native-$version.json
        url="https://raw.githubusercontent.com/pulumi/pulumi-azure-native/v$version/provider/cmd/pulumi-resource-azure-native/schema.json"
        if [ ! -s "$src" ] && ! curl -fsSL -o "$src" "$url"; then
            rm -f "$src"
            echo "FAIL $spec: schema download"
            failed+=("$spec")
            continue
        fi
    fi

    if ! pulumi package gen-sdk "$src" --language rust --out "$out" >"$work/$name.gen.log" 2>&1; then
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
