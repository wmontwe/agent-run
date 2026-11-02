# Helpers for the unit tests. Each test function runs in its own subshell with
# a fresh scratch directory $T, a scratch HOME and Git that ignores the user's
# global and system configuration, so tests never touch real state.

ROOT="$(cd -P -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
for lib in common protect git-guard memory report lock engram; do
    # shellcheck source=/dev/null
    . "$ROOT/lib/$lib.sh"
done

# Scratch Git: no hooks, no signing, a fixed identity.
g() {
    git -c core.hooksPath=/dev/null -c commit.gpgsign=false \
        -c user.name=test -c user.email=test@example.invalid "$@"
}

# new_repo DIR: an empty repository with no hooks.
new_repo() {
    mkdir -p "$1" && git init -q "$1"
}

fail() {
    printf '    FAIL: %s\n' "$*" >&2
    exit 1
}

# assert_ok CMD...: CMD must succeed.
assert_ok() {
    local out
    out="$( ("$@") 2>&1)" || fail "expected success: $* -> $out"
}

# assert_dies TEXT CMD...: CMD must exit nonzero with TEXT in its output.
assert_dies() {
    local text="$1" out rc=0
    shift
    out="$( ("$@") 2>&1)" || rc=$?
    [[ $rc -ne 0 ]] || fail "expected failure: $*"
    [[ "$out" == *"$text"* ]] || fail "expected '$text' in: $out"
}

assert_eq() {
    [[ "$1" == "$2" ]] || fail "expected '$1', got '$2'"
}

# assert_contains TEXT HAYSTACK
assert_contains() {
    [[ "$2" == *"$1"* ]] || fail "expected '$1' in: $2"
}

assert_not_contains() {
    [[ "$2" != *"$1"* ]] || fail "did not expect '$1' in: $2"
}

assert_exists() {
    [[ -e "$1" || -L "$1" ]] || fail "expected $1 to exist"
}

assert_missing() {
    [[ ! -e "$1" && ! -L "$1" ]] || fail "expected $1 to be gone"
}
