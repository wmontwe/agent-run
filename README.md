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
your PATH. Run this **from the agent-run repository**:

```sh
printf '\nexport PATH="%s/bin:$PATH"\n' "$PWD" >> ~/.zshrc
source ~/.zshrc
```

Keep this checkout outside any workspace you hand to an agent. Alternatively,
symlink `bin/agent` into a directory already on your PATH.

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

Existing top-level `.idea` is read-only and `.signing` is hidden. Agents can
read everything else in the workspace, including secrets outside `.signing`
and committed history. Keep secrets out of workspaces and shared knowledge or
memory. Review the launcher's post-run report and inspect agent changes on the
host before running tools or committing them; the report is advisory.

Memory is for notes: only directories and regular `.md`/`.jsonl` files with
safe, non-hidden names remain there. Other entries are moved to
`~/.agent-run/quarantine` before and after a run. To version memory, initialize
it on the host and then explicitly trust its `.git`:

```sh
git -C ~/.agent-run/memory init
agent trust-memory
```

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
