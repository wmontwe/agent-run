# agent-run

Run coding agents (Claude Code, Codex, Gemini CLI, Pi) in Podman containers,
each strictly isolated from the host and from each other, with a workspace to
work in and an egress firewall in front of it.

- **Podman** isolates each agent: non-root user, read-only root filesystem,
  all capabilities dropped, no host home, no engine socket.
- **Compose** adds the firewall: each agent sits on its own `internal` network
  with no route out. Its only way to the internet is its own stock
  [Squid](https://hub.docker.com/r/ubuntu/squid) proxy, which allows HTTPS to
  the hosts in that agent's allowlist and denies everything else.
- **`bin/agent`** launches agents and protects the host from what they leave
  behind: the workspace's `.git` and IDE configuration are read-only, signing
  keys are hidden, memory is kept to plain notes, and risky changes are
  reported when the agent exits.

```
claude ──(claude network, no route out)── proxy-claude ──┐
codex  ──(codex network,  no route out)── proxy-codex  ──┼── egress ── allowed hosts
...                                                      ┘
```

| Per agent (isolated) | Shared on purpose |
| --- | --- |
| Image with only that agent's CLI | Workspace (`/workspace`, `.git` read-only) |
| Home: `~/.agent-run/<agent>/home` | Knowledge (`/knowledge`, read-only) |
| `/tmp` and `/var/tmp`: new and empty every run | Memory (`/memory`, writable) |
| Internal network and proxy | `proxy/squid.conf` rules |
| Allowlist (`proxy/allowlists/<agent>.txt`) | |

## Usage

```sh
mkdir -p ~/.agent-run/{knowledge,memory}   # once
mkdir -p ~/.agent-run/claude/home          # once per agent
podman compose build claude
ln -s "$PWD/bin/agent" ~/bin/agent          # once; any folder on your PATH
```

Then, from any Git repository:

```sh
cd ~/projects/my-app
agent claude          # starts Claude Code; or codex, gemini, pi
agent claude bash     # shell in the Claude container instead
```

The current directory is the workspace (set `WORKSPACE` to use another).
Nothing is added to the repository. Logins, knowledge, memory and proxies are
the same whichever repo you run from, so you sign in once per agent.

The symlink may live anywhere; what runs is its target in this repo, which
agents cannot reach. `bin/agent` refuses to start when the workspace:

- has no `.git` directory, or its `.git` is a file or symlink (worktrees and
  submodule checkouts are not supported),
- has Git settings that load files from the workspace or memory:
  `core.hooksPath`, `include.path` or `includeIf.*.path` in any config scope
  pointing into them (outside `.git`), such as a husky `core.hooksPath=.husky`,
  or a command setting (`core.fsmonitor`, `filter.*`, `diff.*`, aliases, …)
  that names a script there,
- has a `.idea` or `.signing` that is a symlink or a file, or a `.idea` that
  contains a symlink,
- has a file in `.git`, `.idea` or `.signing` that is hard-linked to a file
  elsewhere in the workspace or memory (the other name would be writable),
- overlaps this repo or any folder mounted into the container (so `$HOME` and
  `~/.agent-run` are refused too),
- has `:` `,` `*` `?` `[` `]` `\` or a newline in its path,
- or the Podman connection is rootful (see [Caveats](#caveats)).

`podman compose run --rm claude` still works, but without any of the
protections in [Workspace protection](#workspace-protection) and
[Knowledge and memory](#knowledge-and-memory): `.git` is writable, nothing is
hidden or reported, and `/tmp` is Podman's default in-memory one.

### Sign-in and cleanup

Sign in inside the container; `BROWSER=echo` prints login URLs instead of
opening a browser. Each agent's home (logins, settings) is a plain folder on
your Mac, `~/.agent-run/<agent>/home` → `/home/node`, so you can inspect and
edit it directly. It contains login tokens; keep it out of backups you share.
To reset an agent, delete its folder (then recreate the empty `home` folder).
`podman compose down` stops the proxies.

`/tmp` and `/var/tmp` are anonymous volumes on the Podman VM's disk, not in
memory and not on your Mac. Each container gets its own, never shared between
agents or runs, and they are removed with the container: every run starts
empty, however much an agent wrote last time. There is no size cap, so during a
run they can fill the VM disk. Volumes left by a crash show up in
`podman volume ls` and are removed by `podman volume prune`.

### Limits and versions

Each agent container is limited to 4 GB of memory, 4 CPUs and 1024 processes
(`mem_limit`, `cpus` and `pids_limit` in `compose.yaml`), so one agent can't
starve the VM the others share. Raise them there if an agent needs more.

Everything is pinned, so a rebuild never silently picks up something new:

- the base image (`Dockerfile`) and the Squid proxy image (`proxy/Dockerfile`,
  Ubuntu 26.04 LTS) by digest,
- each agent CLI and all its dependencies, with checksums, in
  `agents/<agent>/package-lock.json`, installed with `npm ci`.

npm install scripts never run, and Claude Code's own updater is off
(`DISABLE_AUTOUPDATER`). Once the repository is on GitHub, Dependabot
(`.github/dependabot.yml`) proposes updates for all of these weekly, each
release at least 7 days old. After merging an update, rebuild:

```sh
podman compose build claude
```

To update an agent by hand, pin the new version in its lockfile (scripts are
disabled by the folder's `.npmrc`) and rebuild:

```sh
cd agents/claude && npm install --package-lock-only @anthropic-ai/claude-code@<version>
```

## Firewall

Each agent has its own allowlist in `proxy/allowlists/<agent>.txt` (one host
per line; `.example.com` also allows subdomains). After editing:

```sh
podman compose restart proxy-claude
```

See what was blocked:

```sh
podman compose logs -f proxy-claude | grep DENIED
```

Only HTTPS `CONNECT` to port 443 is allowed. Plain HTTP, other ports, private
address ranges and direct connections that skip the proxy are refused. Agents
cannot resolve external DNS names, since internal networks do not forward
lookups, and cannot reach other agents' containers or proxies. Names are
matched as given, never by reverse DNS, so a bare IP is refused even if its
PTR record claims an allowed name. This is a destination allowlist, not TLS
inspection: anything can still be sent to an allowed host.

The proxies are hardened like the agents: Squid runs as an unprivileged user
with a read-only root filesystem, no capabilities and its own resource limits
(256 MB, 64 processes, 1 CPU). They are still the only containers with a
route out, and that network can also reach your Mac (through
`host.containers.internal`, including services listening on it) and the
Podman VM. Squid refuses those destinations, so only a compromised Squid
could use them.

## Workspace protection

### Read-only Git

Agents can read history, diffs and blame, but cannot stage, commit, switch
branches or change hooks and Git configuration: `bin/agent` mounts the
workspace's `.git` read-only over the writable workspace. You commit on the
host after review.

- **Guaranteed:** agents cannot modify `<workspace>/.git` or anything beneath
  it (including submodule metadata in `.git/modules`) through the mounts
  agent-run supplies, including through hard links, and host Git's hooks and
  included configuration never come from the workspace or memory: `bin/agent`
  refuses to start otherwise.
- **Best effort:** Git settings that run commands (`core.fsmonitor`,
  `filter.*`, `diff.*`, editors, credential helpers, `!` aliases, …) are
  refused when a word in them with a slash or a leading `~` points into the
  workspace or memory. A shell command can still reach a script there in ways
  this can't see (variables, `cd` first, a bare name found via `PATH`), so
  don't point such settings at anything an agent can write.
- **Not guaranteed:** everything else in the workspace is untrusted. Host
  tools may load agent-written files, even while the agent is still running.

### IDE configuration and signing keys

Two folders at the top of the workspace get extra protection:

- **`.idea`** (IDE configuration) is **read-only**: agents can read it but not
  change it, since run configurations can execute code. Keep secrets out of it.
  It may not contain symlinks, or files hard-linked to files elsewhere in the
  workspace or memory, since either would leave that file writable.
- **`.signing`** (signing keys) is **hidden**: an existing one is covered by an
  empty read-only folder, so agents can neither read nor change it.

A missing folder is not created, since that would leave an empty folder in
your repository; if an agent creates one, the change report lists it. Only
these exact top-level folders are protected: a keystore anywhere else in the
workspace is visible to the agent, and so is anything ever committed to Git
(`git show HEAD:<path>`), so never commit `.signing`.

### Change report

When the agent exits, `bin/agent` prints an advisory change report. It is
found with `find`, not Git, so `.gitignore` cannot hide anything from it:

- all nested `.git` entries, whatever their age (host Git would use their
  config and hooks when you work inside them),
- created, changed or deleted files that host tools may load or execute:
  `.envrc`, `.env`/`.env.*`, `.mcp.json`, `.gitmodules`, `.gitattributes`,
  `.gitignore`, `git-town.toml`, `.git-town.toml`, `.pre-commit-config.yaml`,
  `lefthook.yml`, `.npmrc`, `.yarnrc.yml`, `mise.toml`, `.tool-versions`,
  `.nvmrc`, `Makefile`, `package.json`, the Gradle and Maven wrappers
  (`gradlew`, `gradle-wrapper.properties`, `gradle-wrapper.jar`, `mvnw`), and
  anything in `.claude/`, `.codex/`, `.gemini/`, `.vscode/`, `.idea/`,
  `.signing/` and `.husky/` (the full list is in `lib/report.sh`),
- the number of other changed files.

The report can be incomplete. Deletions are only tracked for the files above,
and a failed directory traversal is reported as "scan incomplete". Review the
changes on the host with:

```sh
git status --untracked-files=all --ignored
git --no-pager diff --no-ext-diff --no-textconv
```

Plain `git diff` misses untracked and ignored files, and the flags disable the
diff and textconv drivers that `.gitattributes` can select. They do not disable
every way Git can run commands.

## Knowledge and memory

Both are shared by every agent, but only memory is writable:

| | Host folder | In container | Agents can |
| --- | --- | --- | --- |
| **Knowledge** | `~/.agent-run/knowledge` | `/knowledge` | read |
| **Memory** | `~/.agent-run/memory` | `/memory` | read and write |

Knowledge is curated by you: docs, conventions, instructions. Memory is where
agents record what they learn, and what one agent writes the others can read.
Review memory from time to time and move what's worth keeping into knowledge.
Both live outside this repo, so they survive re-cloning it; make
`~/.agent-run/knowledge` its own git repo if you want it versioned. Don't put
secrets in either folder.

Tell each agent about both from its instructions file, e.g. in
`~/.claude/CLAUDE.md`: "Read /knowledge before starting. Save anything worth
remembering to /memory as .md or .jsonl files; subfolders are fine."

Memory may only hold plain notes, so it never becomes something host tools
run or load. Before and after each run, `bin/agent` moves everything else out
of `~/.agent-run/memory` into `~/.agent-run/quarantine/<time>-<pid>-<phase>/`,
keeping its path, and lists it. Nothing is deleted or followed.

Other agents may be running and writing to the same memory, so while the check
runs, every container with memory mounted is paused (for a second or two; an
agent you're working with in another terminal freezes briefly), and agents
starting at that moment wait for it. The check itself runs in a throwaway
container that sees only memory and the quarantine folder, so it can never
move files elsewhere on your Mac. Allowed:

- directories and regular `.md` and `.jsonl` files, nested freely,
- names made of letters, digits, spaces, `.`, `_` and `-`, not starting with
  `.` or `-` (so no `.obsidian/`, `.git/` or other dotfiles).

`.json` is not allowed: too many tools load configuration from it
(`package.json`, `tsconfig.json`, `.vscode/settings.json`, `.mcp.json`).
Symlinks, FIFOs and other special files are always quarantined.

### Memory as a Git repository

Make memory a Git repository and commit it on the host, so every change an
agent makes, including files the quarantine moved out, shows in `git status`
until you commit it:

```sh
git -C ~/.agent-run/memory init
agent trust-memory
```

`agent trust-memory` records this `.git` (by its inode) in
`~/.agent-run/memory.git-id`, outside every mount. Only that `.git` is
trusted: an agent can create a `.git` in a memory folder that isn't a
repository, so `bin/agent` refuses to start when memory has a `.git` that
doesn't match the record (or a record without a `.git`). Inspect it before
running `agent trust-memory` again.

`bin/agent` then treats it like the workspace: its `.git` is mounted
read-only, and an existing `.gitignore` and `.gitattributes` at its top are
read-only too and left alone by the quarantine (agent-created ones are still
quarantined). It refuses to start if memory's `.git`, `.gitignore` or
`.gitattributes` is a symlink, or if either repository's Git configuration
points hooks or includes into the workspace or memory. The change report ends
with the commands to review memory before you commit it.

Between checks, running agents can of course write to memory, and file rules
say nothing about content: whatever one agent writes, the others read as
instructions, so keep reviewing memory.

## MCP servers

- **Inside the agent container** (stdio servers launched by the agent): works
  if the server is installed in the image. Anything it fetches, including
  `npx` downloads from `registry.npmjs.org`, must be in that agent's allowlist.
- **As a separate Compose service** (HTTP servers): add a service on one
  agent's network (see the commented example in `compose.yaml`) and point the
  agent at `http://<service>:<port>`. Only that agent can reach it, and its
  traffic goes through that agent's proxy.

## Development

`bin/agent` orchestrates; the checks live in `lib/`. Run the tests after any
change:

```sh
tests/run.sh           # unit tests, no Podman needed
tests/integration.sh   # against the Podman machine (needs the claude image)
```

See [AGENTS.md](AGENTS.md) for the architecture, invariants and conventions.

## Caveats

Workspace content stays untrusted even with a read-only `.git`: build scripts,
`.envrc`, IDE and agent configuration and Git attributes can all run code when
host tools use them. Review changes before running anything on the host.
Secrets outside `.signing`, such as a `.env` or a keystore elsewhere in
the workspace, are readable by the agent and can be sent to any allowed host;
keep them out of workspaces you hand to agents.

The Podman machine must be rootless (`podman machine set --rootful=false`).
Rootful Podman ignores `keep-id`, so there is no user namespace: the agent's
user is a real VM user and a container escape lands as VM root, which can
read and write your Mac's home through the VM's `/Users` share. `bin/agent`
refuses a rootful connection. Images and volumes are stored separately per
mode, so rebuild the images after switching. Containers share the Podman VM's
kernel; this is not a guarantee against container escapes.
