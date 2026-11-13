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
    podman logs "$container" | grep '^target 0 [0-9][0-9]* [0-9][0-9]*$'
    podman rm "$container" >/dev/null
}

case "$name" in
    agent-claude|agent-codex|agent-gemini|agent-pi)
        gate /usr/local/bin/agent-run-gate
        cli=${name#agent-}
        run "/opt/agent/node_modules/.bin/$cli" --version
        ;;
    build)
        gate /usr/local/bin/agent-run-gate
        for major in 11 17 21 25; do
            output=$(run "/opt/jdk/$major/bin/java" -XshowSettings:properties -version 2>&1)
            printf '%s\n' "$output"
            printf '%s\n' "$output" | grep -E "java.specification.version = $major$"
        done
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
