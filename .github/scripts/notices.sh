#!/usr/bin/env bash
# Write THIRD-PARTY-NOTICES.md, or check it. `just notices` runs this to
# write it: cargo-about reads Cargo.lock through about.toml and about.hbs for
# the crates the released binary is built from, the notices of the two
# embedded fonts follow, and the file is stamped with a digest of everything
# it was generated from. `just notices-check` runs it with --check, which
# fails when that digest is not the current one, so a dependency change
# cannot ship without its notice; the check needs no cargo-about and no
# build. Line endings are normalised, since a few crates ship their notice
# with CRLF.
set -euo pipefail

cd "$(dirname "$0")/../.."

notices=THIRD-PARTY-NOTICES.md
fonts=apps/desktop/assets/fonts
# What the file is generated from: the lockfile, the manifests that choose
# features and targets, the cargo-about configuration and template, the font
# licences and this script.
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
generated=$(mktemp)
trap 'rm -f "$generated"' EXIT
cargo about generate --config about.toml --manifest-path apps/desktop/Cargo.toml \
    --locked --offline --fail about.hbs --output-file "$generated"

{
    sed "s/@INPUTS_DIGEST@/$digest/" "$generated"
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
