#!/bin/bash
# Runs the unit tests: every test_* function in tests/unit/*_test.sh, each in
# its own subshell with scratch state. No Podman needed. Uses macOS /bin/bash
# 3.2 on purpose, like bin/agent.
#
#   tests/run.sh                 all tests
#   tests/run.sh git_guard       only tests/unit/git_guard_test.sh
#
# The Podman checks are separate: tests/integration.sh.
set -euo pipefail

here="$(cd -P -- "$(dirname -- "$0")" && pwd -P)"
# Physical path, like bin/agent's: on macOS the temp folder is behind a
# symlink (/var -> /private/var), and the libraries compare canonical paths.
scratch="$(cd -P -- "$(mktemp -d "${TMPDIR:-/tmp}/agent-run-tests.XXXXXX")" && pwd -P)"
trap 'chmod -R u+rwx "$scratch" 2>/dev/null; rm -rf -- "$scratch"' EXIT

passed=0
failed=0
for file in "$here"/unit/*_test.sh; do
    suite="$(basename "$file" _test.sh)"
    [[ $# -eq 0 || " $* " == *" $suite "* ]] || continue
    echo "$suite"
    tests="$(bash -c '. "$1"; declare -F | awk "\$3 ~ /^test_/ {print \$3}"' _ "$file")"
    for test in $tests; do
        T="$scratch/$suite/$test"
        mkdir -p "$T/home"
        if (
            set -euo pipefail
            export T HOME="$T/home" GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
            cd "$T"
            # shellcheck source=/dev/null
            . "$here/helpers.sh"
            # shellcheck source=/dev/null
            . "$file"
            "$test"
        ) >"$T.log" 2>&1; then
            passed=$((passed + 1))
            echo "  ok   $test"
        else
            failed=$((failed + 1))
            echo "  FAIL $test"
            sed 's/^/       /' "$T.log"
        fi
    done
done
echo "$passed passed, $failed failed"
[[ $failed -eq 0 ]]
