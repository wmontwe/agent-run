#!/bin/sh
# CI: installs Rust TOOLCHAIN [COMPONENT...] with rustup (installing rustup first if the
# runner has none) and makes it the default.
set -eu
toolchain=$1
shift
if ! command -v rustup >/dev/null; then
    curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
    echo "$HOME/.cargo/bin" >> "$GITHUB_PATH"
    PATH=$HOME/.cargo/bin:$PATH
fi
for component in "$@"; do
    set -- "$@" --component "$component"
    shift
done
rustup toolchain install "$toolchain" --profile minimal "$@"
rustup default "$toolchain"
