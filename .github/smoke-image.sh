#!/bin/sh
# CI runtime checks after building an image.
set -eu
name=${1:?usage: smoke-image.sh IMAGE_NAME}
image="localhost/agent-run-$name"
container="agent-run-smoke-$name-$$"
trap 'podman rm --force "$container" >/dev/null 2>&1 || true' EXIT
trap 'exit 1' HUP INT TERM

run() {
    entrypoint=$1
    shift
    podman run --rm --network none --entrypoint "$entrypoint" "$image" "$@"
}

gate() {
    binary=$1
    # Hold a real command, then release it and inspect its output and exit status.
    podman run -d --network none --name "$container" --entrypoint "$binary" \
        "$image" run -- "$binary" report / >/dev/null
    test "$(podman inspect --format '{{.State.Running}}' "$container")" = true
    test -z "$(podman logs "$container")"
    podman exec "$container" "$binary" release
    test "$(podman wait "$container")" = 0
    podman logs "$container" | grep '^target 0 [0-9][0-9]* [0-9][0-9]* [0-9][0-9]*$'
    podman rm "$container" >/dev/null
}

case "$name" in
    agent-claude|agent-codex|agent-gemini|agent-pi)
        gate /usr/local/bin/agent-run-gate
        cli=${name#agent-}
        run "/opt/agent/node_modules/.bin/$cli" --version
        run /bin/sh -c 'command -v cargo-run && command -v kotlin-run && ! command -v cargo && ! command -v rustc && ! command -v java'
        ;;
    build)
        gate /usr/local/bin/agent-run-gate
        for major in 11 17 21 25; do
            output=$(run "/opt/jdk/$major/bin/java" -XshowSettings:properties -version 2>&1)
            printf '%s\n' "$output"
            printf '%s\n' "$output" | grep -E "java.specification.version = $major$"
        done
        # Compose Desktop's Skiko loads EGL (Compose tests in Kotlin Toolchain builds).
        run sh -c 'ldconfig -p | grep -q "libEGL.so.1 "'
        # Compile, test, format and lint without network or writable image toolchains.
        podman run --rm --network none --read-only --tmpfs /tmp:rw,exec \
            --entrypoint /bin/sh "$image" -ec '
                export CARGO_HOME=/tmp/cargo CARGO_TARGET_DIR=/tmp/target
                mkdir -p /tmp/smoke/src
                cd /tmp/smoke
                printf "[package]\nname = \"rust-smoke\"\nversion = \"0.1.0\"\nedition = \"2024\"\n" > Cargo.toml
                printf "fn main() {\n    println!(\"rust smoke\");\n}\n" > src/main.rs
                cargo --version
                rustc --version
                cargo test --offline
                cargo fmt --all --check
                cargo clippy --offline --all-targets -- -D warnings
            '
        run node /opt/browser/node_modules/@playwright/mcp/cli.js --help
        # Exercise Playwright's packaged runtime and the system Chromium without downloads.
        run node -e '
            const { chromium } = require("/opt/browser/node_modules/playwright");
            (async () => {
                const browser = await chromium.launch({
                    executablePath: "/usr/bin/chromium",
                    headless: true,
                    args: ["--no-sandbox", "--disable-dev-shm-usage"]
                });
                try {
                    const page = await browser.newPage();
                    await page.setContent("<h1>agent-run smoke</h1>");
                    if (await page.textContent("h1") !== "agent-run smoke") {
                        throw new Error("Chromium did not render the smoke page");
                    }
                } finally {
                    await browser.close();
                }
            })().catch(error => { console.error(error); process.exit(1); });
        '
        ;;
    gate)
        gate /agent-run-gate
        ;;
    proxy)
        run /usr/sbin/squid-gnutls -v
        ;;
    github-relay)
        run /usr/sbin/nginx -v
        ;;
    mirror)
        run java -version
        ;;
    *)
        echo "unknown image: $name" >&2
        exit 2
        ;;
esac
