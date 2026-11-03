# agent-run

Run Claude Code, Codex, Gemini CLI or Pi in rootless Podman containers. Each
agent gets an isolated network and an HTTPS destination allowlist. The launcher
mounts the workspace's `.git` read-only, protects top-level `.idea` and
`.signing`, checks shared memory, and reports sensitive changes after a run.

## Set up

Install Podman and podman-compose, and start a **rootless** Podman machine.
From this repository:

```sh
mkdir -p ~/.agent-run/{knowledge,memory}
mkdir -p ~/.agent-run/{claude,codex,gemini,pi}/home  # or only the agents you use
podman compose build claude                 # replace with codex, gemini or pi
```

To make `agent` available in zsh, add this repository's `bin` directory to
your PATH. If your zsh uses `ZDOTDIR`, its startup file is `$ZDOTDIR/.zshrc`,
not `~/.zshrc`. Run this **from the agent-run repository**:

```sh
printf '\nexport PATH="%s/bin:$PATH"\n' "$PWD" >> "${ZDOTDIR:-$HOME}/.zshrc"
source "${ZDOTDIR:-$HOME}/.zshrc"
command -v agent
```

Keep this checkout outside any workspace you hand to an agent. Remove any
existing `agent()` shell function: functions take precedence over PATH
executables. Alternatively, symlink `bin/agent` into a directory already on
your PATH.

## Run

From a Git repository with a real `.git` directory:

```sh
cd ~/projects/my-app
agent claude          # or codex, gemini, pi
agent claude bash     # run a shell instead of the agent CLI
```

The current directory is mounted at `/workspace`; set `WORKSPACE` to use
another directory. Sign in inside the container (`BROWSER=echo` prints login
URLs). Credentials and settings persist in `~/.agent-run/<agent>/home`;
protect that folder. The launcher refuses worktrees, rootful Podman, unsafe
Git configuration and workspaces that overlap its own checkout or other
mounts. Use `agent`, not `podman compose run`, which bypasses the launcher
protections.

| Host path | Container path | Access |
| --- | --- | --- |
| Git workspace | `/workspace` | Writable except `.git` and protected folders |
| `~/.agent-run/<agent>/home` | `/home/node` | Writable; per-agent credentials |
| `~/.agent-run/knowledge` | `/knowledge` | Read-only; shared by agents |
| `~/.agent-run/memory` | `/memory` | Writable; shared by agents |

Agents can't reach your clipboard. To copy their output, select with your
terminal: hold Shift while selecting (Option in Terminal.app and iTerm2).

Git in the container uses a read-only copy of your global gitignore.
Existing top-level `.idea` is read-only and `.signing` is hidden. Agents can
read everything else in the workspace, including secrets outside `.signing`
and committed history. Keep secrets out of workspaces and shared knowledge or
memory. Review the launcher's post-run report and inspect agent changes on the
host before running tools or committing them; the report is advisory.

Memory is for notes: only directories, regular `.md`/`.jsonl` files with
safe, non-hidden names, and empty `.gitkeep` files remain there. Other entries are moved to
`~/.agent-run/quarantine` before and after a run. To version memory, initialize
it on the host and then explicitly trust its `.git`:

```sh
git -C ~/.agent-run/memory init
agent trust-memory
```

## Builds

Agents can build Gradle and Android projects. A project's tools come from its
`mise.toml`, which needs a `mise.lock` covering Linux; create it on the host:

```sh
mise lock --platform linux-arm64,macos-arm64
```

Tools, the Gradle cache and the Android SDK are kept per agent in Podman
volumes, so only the first build downloads them. Android SDK licenses are
taken from `~/Library/Android/sdk/licenses` (or `$ANDROID_HOME`). Large builds
need more memory than the default 4 GB per agent (the launcher prints the
limit at start), and a Podman machine to match:

```sh
podman machine set --memory 24576   # with the machine stopped
AGENT_MEMORY=12g agent claude
```

Build hosts every agent may reach are listed in `proxy/allowlists/build.txt`.

## Engram

Projects linked to an [engram](https://github.com/wmontwe/engram-core) context
repository get their context in the container: the project's package is
read-only, its `.memory` is writable for project notes, and engram-core's
skills are available. If the project commits its own `AGENTS.md`, agents see
engram's instead. Agents can't save; review the notes and run
`engram save <project>` on the host, as the post-run report shows. Set
`AGENT_ENGRAM=0` to run without it.

## Network and maintenance

Each agent's HTTPS destinations are listed in
`proxy/allowlists/<agent>.txt`. To apply edits and inspect blocked requests:

```sh
podman compose restart proxy-claude
podman compose logs -f proxy-claude | grep DENIED
```

Only HTTPS to allowed hosts on port 443 is available; allowed hosts can still
receive anything the agent sends them. To rebuild after updating this checkout
or its pinned dependencies, run `podman compose build <agent>`. To stop proxies,
run `podman compose down` from this repository.

For implementation details and tests, see [AGENTS.md](AGENTS.md).
