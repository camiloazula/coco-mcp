#!/usr/bin/env bash
# Fail when a change carries attribution to anyone but the people who wrote
# it: a co-author trailer for an address that is not a GitHub account's
# noreply address, a "Generated with/by" line, or an author or committer on
# a noreply address of some other service. Commit messages, the author and
# committer of each commit in <range> are read, and the pull request's title
# and body from PR_TITLE and PR_BODY, since a squash merge builds the
# message from them. `just check-attribution [range]` runs it locally.
set -euo pipefail

range=${1:-origin/main..HEAD}
failed=0

fail() {
    echo "::error::$1"
    failed=1
}

# check_text <where> <text>: the lines of one message.
check_text() {
    local where=$1 line email
    while IFS= read -r line; do
        if [[ $line =~ ^[[:space:]]*[Cc]o-[Aa]uthored-[Bb]y: ]]; then
            email=${line##*<}
            if [[ $email != *@users.noreply.github.com'>'* ]]; then
                fail "$where: co-author trailer for something other than a GitHub account: $line"
            fi
        elif [[ $line =~ ^[[:space:]]*(🤖[[:space:]]*)?[Gg]enerated[[:space:]]+(with|by)([[:space:]]|$) ]]; then
            fail "$where: attribution line: $line"
        fi
    done <<<"$2"
}

# check_identity <where> <name> <email>
check_identity() {
    local email
    email=$(printf %s "$3" | tr '[:upper:]' '[:lower:]')
    if [[ $email == noreply@* && $email != noreply@github.com ]]; then
        fail "$1: $2 <$3> is a noreply address of another service"
    fi
}

check_text "pull request title" "${PR_TITLE:-}"
check_text "pull request body" "${PR_BODY:-}"

for sha in $(git rev-list --no-merges "$range"); do
    short=$(git rev-parse --short "$sha")
    check_text "commit $short" "$(git show -s --format=%B "$sha")"
    check_identity "commit $short author" "$(git show -s --format=%an "$sha")" "$(git show -s --format=%ae "$sha")"
    check_identity "commit $short committer" "$(git show -s --format=%cn "$sha")" "$(git show -s --format=%ce "$sha")"
done

exit "$failed"
