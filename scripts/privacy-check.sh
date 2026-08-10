#!/usr/bin/env bash
# Reject known personal identifiers from tracked, non-binary repository text.

set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(git -C "${SCRIPT_DIR}/.." rev-parse --show-toplevel)"
cd "${PROJECT_ROOT}"

failed=0

check_rule() {
    local label="$1"
    local expression="$2"
    local matches
    local status

    if matches="$(git grep -n -I -i -E -e "${expression}" -- .)"; then
        printf 'Privacy guard: found %s:\n%s\n' "${label}" "${matches}" >&2
        failed=1
    else
        status=$?
        if ((status != 1)); then
            printf 'Privacy guard: git grep failed while checking %s.\n' "${label}" >&2
            exit "${status}"
        fi
    fi
}

# Keep the expressions split so the guard scans its own tracked source without
# having to exempt itself. These are narrow, previously exposed values: private
# address ranges in general remain valid test data, and documentation-only
# ranges such as 192.0.2.0/24 remain allowed.
check_rule "a known personal home path" '/home/'"yj"
check_rule "a known personal SSH login" \
    '(^|[^[:alnum:]_-])'"yj"'@'
check_rule "a known private IPv4 endpoint" \
    '(^|[^0-9])(10[.]21[.]31[.]17|10[.]68[.]18[.]60|100[.]99[.]153[.]18|192[.]168[.]0[.]61)([^0-9]|$)'
check_rule "a known private host alias" \
    '(^|[^[:alnum:]_-])('"cloud"'-dev|'"home"'-dev|'"my"'ubuntu|dev'"-60"')([^[:alnum:]_-]|$)'

if ((failed)); then
    printf '%s\n' \
        'Replace personal data with neutral names and RFC 5737 documentation addresses.' >&2
    exit 1
fi

printf 'Privacy guard passed: tracked text contains no known personal identifiers.\n'
