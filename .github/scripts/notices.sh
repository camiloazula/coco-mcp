#!/usr/bin/env bash
# Write THIRD-PARTY-NOTICES.md, or check it. `just notices` runs this to
# write it: cargo-about reads Cargo.lock through about.toml and about.hbs for
# the crates the released binary is built from and which licence each is
# taken under; each licence's text is then fetched once from the SPDX
# licence list at a pinned release, followed by the crates under it; the
# notices of the two embedded fonts close the file, which is stamped with a
# digest of everything it was generated from. `just notices-check` runs it
# with --check, which fails when that digest is not the current one, so a
# dependency change cannot ship without its notice; the check needs no
# cargo-about, no network and no build.
set -euo pipefail
export LC_ALL=C

cd "$(dirname "$0")/../.."

notices=THIRD-PARTY-NOTICES.md
fonts=apps/desktop/assets/fonts
# The SPDX licence list release the texts come from. Its texts do not
# change between releases; the pin keeps a regeneration from depending on
# the day it runs.
spdx=https://raw.githubusercontent.com/spdx/license-list-data/v3.29.0/text
# What the file is generated from: the lockfile, the manifests that choose
# features and targets, the cargo-about configuration and template, the font
# licences and this script (which names the SPDX release).
inputs=(Cargo.lock Cargo.toml apps/*/Cargo.toml crates/*/Cargo.toml about.toml about.hbs
    "$fonts"/LICENSE-*.txt "$0")

digest=$(cat "${inputs[@]}" | shasum -a 256 | cut -c1-16)
stamp="Inputs digest \`$digest\`"

if [[ ${1:-} == --check ]]; then
    if [[ -f $notices ]] && grep -qF "$stamp" "$notices"; then
        exit 0
    fi
    echo "::error::$notices is out of date: run \`just notices\` and commit it" >&2
    exit 1
fi

# Every crate of the lockfile, since the file covers every released target,
# not only this machine's; cargo-about then needs no network.
cargo fetch --locked
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cargo about generate --config about.toml --manifest-path apps/desktop/Cargo.toml \
    --locked --offline --fail about.hbs --output-file "$work/generated.md"

# The template ends with one `@@ id|name|crate version` line per crate and
# licence it is taken under, unsorted and repeated where a crate ships its
# licence in several files.
grep '^@@ ' "$work/generated.md" | cut -c4- | sort -u > "$work/under"

{
    sed -e "s/@INPUTS_DIGEST@/$digest/" -e '/^@@ /d' "$work/generated.md"
    printf '## Licences\n'
    cut -d'|' -f1,2 "$work/under" | sort -u | while IFS='|' read -r id name; do
        curl -fsSL "$spdx/$id.txt" -o "$work/text" \
            || { echo "::error::no SPDX text for $id at $spdx" >&2; exit 1; }
        printf '\n### %s (%s)\n\nCrates under it:\n\n' "$name" "$id"
        awk -F'|' -v id="$id" '$1 == id { print "- " $3 }' "$work/under"
        printf '\n```text\n'
        tr -d '\r' < "$work/text"
        printf '\n```\n'
    done
    printf '\n## Fonts\n\n'
    printf 'Inter and Geist Mono are embedded in the binary, both under the SIL Open\n'
    printf 'Font License 1.1, as their notices say:\n'
    for licence in "$fonts"/LICENSE-*.txt; do
        printf '\n```text\n'
        cat "$licence"
        printf '\n```\n'
    done
} | tr -d '\r' > "$notices"
echo "wrote $notices ($(wc -l < "$notices" | tr -d ' ') lines, digest $digest)"
