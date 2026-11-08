#!/bin/bash
# Entrypoint of every agent image. Starts a virtual display, so GUI apps
# (Compose Desktop, a Chromium window) run and can be screenshotted with
# `scrot`, and registers the Playwright MCP server with the agent's CLI when
# that CLI is what starts. /tmp is per container, so the display is too.
export DISPLAY=:99
if [ ! -e /tmp/.X11-unix/X99 ]; then
    # No TCP listener: only processes in this container reach the display.
    Xvfb :99 -screen 0 1920x1080x24 -nolisten tcp >/tmp/xvfb.log 2>&1 &
    for _ in $(seq 40); do
        [ -e /tmp/.X11-unix/X99 ] && break
        sleep 0.05
    done
fi

# User scope in the agent's own home, through each CLI's own command, so the
# user's other MCP servers stay. Gemini reads /etc/gemini-cli/settings.json;
# Pi has no MCP.
register() {
    "$@" >/dev/null 2>&1 || echo "agent-run: could not register the Playwright MCP server" >&2
}
case "${1:-}:$AGENT_NAME" in
    claude:claude)
        claude mcp get playwright >/dev/null 2>&1 ||
            register claude mcp add --scope user playwright -- browser-mcp ;;
    codex:codex)
        codex mcp get playwright >/dev/null 2>&1 ||
            register codex mcp add playwright -- browser-mcp ;;
esac
exec "$@"
