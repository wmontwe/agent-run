# AGENTS.md

Guidance for AI agents working on this repository. User-facing documentation is
[README.md](README.md); keep the two consistent when behavior changes.

## What this project is

agent-run runs coding agents (Claude Code, Codex, Gemini CLI, Pi) in rootless
Podman containers on macOS, each isolated from the host and from each other.
The threat model: **the agent inside the container is untrusted.** Everything
here exists to limit what it can reach, and what it can leave behind for the
host to execute later.

## Layout

| Path | Role |
| --- | --- |
| `bin/agent` | Hardened launcher and the only supported entry point. The orchestrator: holds the configuration (`hidden`, `readonly_dirs`, mount sources) and the order of the checks. Bash. |
| `lib/common.sh` | `die`, `canonical`, `overlaps`, `check_path_chars`, `resolve_path`. |
| `lib/protect.sh` | `.git` and protected-folder checks, symlinks in `.idea`, memory repository detection, hard links. |
| `lib/git-guard.sh` | The Git config guard (hooks, includes, command settings). |
| `lib/memory.sh` | Memory quarantine. |
| `lib/lock.sh` | The memory lock (`memory_lock`, `memory_unlock`). |
| `lib/engram.sh` | Engram detection and validation (`detect_engram`). |
| `lib/report.sh` | Snapshot and change report. |
| `lib/podman.sh` | Rootless check, `compose`, container naming and cleanup; the only library that calls Podman. |
| `tests/run.sh`, `tests/unit/` | Unit tests for `lib/`, no Podman needed. |
| `tests/integration.sh` | Checks against the real Podman machine. |
| `compose.yaml` | One agent service plus one Squid proxy service per agent. |
| `Dockerfile` | One image per agent (`ARG AGENT`), with only that CLI installed. Named `Dockerfile` so Dependabot finds it. |
| `agents/<agent>/` | `package.json`, `package-lock.json` and `.npmrc` (`ignore-scripts`, `save-exact`) pinning that agent's CLI. |
| `.github/dependabot.yml` | Weekly updates for npm lockfiles, the base image and the proxy image, with a 7-day cooldown. |
| `proxy/Dockerfile` | The pinned Squid image (Ubuntu 26.04 LTS), built once as `localhost/agent-run-proxy` for every proxy. |
| `proxy/squid.conf` | Shared proxy rules: HTTPS `CONNECT` to port 443 only, no private ranges. |
| `proxy/allowlists/<agent>.txt` | Per-agent destination allowlist. |
| `workspace/` | Default workspace for plain `podman compose run` only. |

Host state lives outside the repo, under `~/.agent-run/`:
- `knowledge/` → `/knowledge`, read-only
- `memory/` → `/memory`, read-write and shared by every agent
- `<agent>/home/` → `/home/node`, logins and settings for that agent
- `quarantine/`, where files `bin/agent` moved out of memory end up. It is never mounted.

## Architecture

- **Container hardening** (`x-agent` in `compose.yaml`):
  - `read_only: true`, `cap_drop: [ALL]`, `no-new-privileges`
  - `userns_mode: keep-id:uid=1000,gid=1000`, so files appear as the host user
  - no engine socket
- **Egress:** each agent is on its own `internal: true` network. Its only route
  out is its own proxy, which is also attached to `egress`, and the proxy is
  reached as `proxy:3128`. Agents can't resolve external DNS names.
- **Proxy hardening** (`x-proxy`): `read_only`, `cap_drop: [ALL]`,
  `no-new-privileges`, Squid as UID 584792, a 1 MB tmpfs for pebble's state
  (`/var/lib/pebble/default`, `U` for ownership), `mem_limit: 256m`,
  `pids_limit: 64`, `cpus: 1`. `bin/agent` starts it with a separate
  `up -d proxy-<agent>` (reused, not recreated, when already running) and
  runs the agent with `--no-deps`, because `--podman-run-args` would also
  apply to dependencies: the proxy used to inherit the agent's `/tmp` volumes.
- **Proxy residual risk:** the `egress` network can reach the Mac
  (`host.containers.internal` = 192.168.127.254, e.g. AirPlay on 5000/7000),
  the VM's sshd (169.254.1.2:22) and the gvproxy gateway (192.168.127.1).
  Squid's `private` ACL denies all of these, so only a compromised Squid
  could use them.
- **Git:** `bin/agent` mounts `<workspace>/.git` read-only over the read-write
  workspace. Agents can never stage or commit; the human commits on the host.
  It also refuses Git settings that make host Git load files from the workspace
  or memory, read with `git --no-pager config --no-includes --show-origin
  --null`. Enforced for `core.hooksPath`, `include.path` and
  `includeIf.*.path`; best effort for command-valued settings
  (`command_keys`: fsmonitor, editors, pagers, diff/filter/merge drivers,
  credential helpers, aliases, …), where only words containing a slash or
  starting with `~` are resolved (`check_git_command`). Includes are followed
  only into files outside both folders; targets inside a protected `.git` are
  allowed, since it is read-only.
- **Hard links:** before launch, files in `.git`, the protected folders and
  memory's protected files that share an inode with a file in any writable
  mount (workspace, memory, the agent's home) are refused (`stat -f '%d:%i'`, compared with `comm`).
  Agents can't create such links at runtime: `link()` across mounts fails
  with `EXDEV`. Links to files outside the mounts (a local clone's objects)
  are allowed. The workspace scan only runs when a protected file has more
  than one link.
- **Rootless only:** `bin/agent` refuses unless `podman info` reports
  `Rootless=true`.
- **Protected folders** (top level only; nested copies are unprotected):
  - `hidden=(.signing)`: an existing folder is covered by an empty read-only
    one from the wrapper's temp folder.
  - `readonly_dirs=(.idea)`: an existing folder is bind-mounted read-only with
    its real contents. It's refused if it contains any symlink (`find -P -type
    l`), since a link into the workspace would leave its target writable.
  - A missing one is not created, since that would leave a mount point in the
    repository, and a symlink or file in its place is refused.
- **Limits and pins:** `mem_limit: 4g`, `cpus: 4`, `pids_limit: 1024` in
  `x-agent`; base (`Dockerfile`) and proxy (`proxy/Dockerfile`) images pinned
  by tag plus multi-arch index digest;
  agent CLIs and all dependencies by checksum in
  `agents/<agent>/package-lock.json`, installed to `/opt/agent` with
  `npm ci --ignore-scripts`; `DISABLE_AUTOUPDATER=1`.
- **/tmp and /var/tmp:** anonymous volumes on the Podman VM disk, one per
  container and removed with it. `/dev/shm` is a 64 MB tmpfs.
- **Memory hygiene:**
  - Before and after each run, `bin/agent` moves anything that isn't a
    directory or a regular `.md` or `.jsonl` file into quarantine. It also
    moves names that start with `.` or `-`, or contain unsafe characters.
  - Paths are kept, symlinks are never followed, and nothing is deleted.
  - While the quarantine runs, nothing else can write to memory: every
    running container with memory mounted is paused (`pause_memory_users`,
    matched by mount source, so plain `podman compose run` containers too)
    and resumed after (`resume_paused`, also in the EXIT trap). The
    quarantine, and each agent launch until its container exists, hold
    `~/.agent-run/memory.lock` (a directory; a dead holder's lock is taken
    over), so an agent can't start unseen in the middle of a quarantine.
    The signal traps are set before anything is paused.
  - The quarantine runs in a throwaway container (`quarantine_in_container`:
    agent image, `--network none`, read-only, no capabilities, only memory,
    the quarantine folder and `lib/` mounted). Memory is shared, so a
    concurrent agent could swap a folder for a symlink between `find` and
    `mv`; on the host that would move host files, in the container it can't
    reach any. Moving across those mounts copies and deletes, so read-only
    folders are made writable first (`chmod -R u+rwX`, ours anyway).
  - Memory's `.git` is only trusted if its `stat -f '%d:%i'` matches
    `~/.agent-run/memory.git-id`, written by `agent trust-memory`. An agent
    can create `/memory/.git` when memory isn't a repository; without the
    record, any `.git` would look like the user's.
  - Memory may be a Git repository committed on the host (`mem_repo`). Its
    `.git`, plus a pre-existing top-level `.gitignore`/`.gitattributes`
    (`mem_protected`), are mounted read-only and skipped by the quarantine;
    `find` prunes memory's `.git` only when it's that real repo, so an
    agent-made `.git` is still quarantined.
  - The Git config guard checks both repositories, each against both
    agent-writable folders (`agent_writable`): a workspace config including
    a memory file is as bad as one including a workspace file.
- **Engram:** when `<ws>/AGENTS.md` is a symlink of exactly the form
  `../<repo>/projects/<project>/AGENTS.md` (`detect_engram`), the context
  repository must be valid (`AGENTS.md`, `projects/`, `engram`,
  `engram-core/`), the package and its `.memory` plain directories, and the
  repository must not overlap the workspace or any mount source. Then:
  - the package is mounted read-only at `/<repo>/projects/<project>`, where
    the workspace's relative overlay links reach it, its `.memory` writable on
    top, and `engram-core/.agents/skills` read-only (the package's skill links
    point there). Never the repository's `.git`, `engram` or engram-core's
    scripts and MCP server; the user saves with `engram save` on the host.
  - its `.memory` is an extra agent-writable folder: in the Git guard
    (`git_guard_init … WRITABLE...`, and the repository's own config is
    checked, since `engram save` runs Git there), the hard-link scan (the
    package is protected; the writable `.memory` inside it is pruned from the
    protected scan), the pause, and the quarantine (to
    `quarantine/engram-<project>`). `engram save` uses `git add -f`, so the
    quarantine is what keeps non-notes out of its commits.
  - Empty `.gitkeep` files are allowed in any memory.
  - Any other `AGENTS.md`, file or link, is a normal project. A link in
    engram's form that leads nowhere valid is refused, never half-mounted.
    `/<repo>` may not be a reserved top-level path (`ENGRAM_RESERVED`).
    `AGENT_ENGRAM=0` skips engram.
  - The workspace's overlay links stay agent-writable; the report flags a
    replaced `AGENTS.md`, `CLAUDE.md`, `.claude/`, `.agents/`, `.codex/`,
    `.junie/`, `.air/` or `.mcp.json`.
- **Post-run report:**
  - advisory and printed to stderr
  - produced with `find`, not git, so `.gitignore` can't hide anything
  - lists nested `.git` entries, plus created, changed or deleted files that
    host tools load (`.envrc`, `.claude/`, `.gitignore`, `git-town.toml`, and so on)

### `bin/agent` flow

The steps below are what `bin/agent` orchestrates; each check is a function in
`lib/` (named in the layout table).

1. **Preflight:**
   - resolve the launcher root through symlinks
   - canonicalize `WORKSPACE` (default `$PWD`) once
   - reject unsafe characters in the path
   - require `.git` to be a real directory, testing `-L` before `-d`
   - require a rootless Podman connection
   - refuse a protected folder that is a symlink or a file, and a read-only
     folder that contains a symlink
   - canonicalize every mount source and refuse any overlap with the workspace
   - after the temp folder exists, refuse Git hooks paths or includes that
     resolve into the workspace (fail closed if Git can't read its config)
2. Run `mktemp -d`, check it doesn't overlap the workspace or any mount source,
   and create `empty/` in it as the source for the hidden-folder mounts.
3. Take the memory lock, run the memory quarantine (`before`, in its
   container, with every other memory user paused), and keep the lock.
4. `cd` to the launcher root, start the proxy alone (`up -d proxy-<agent>`),
   take the snapshot of sensitive workspace paths and the `start` timestamp,
   and run
   `podman compose --env-file /dev/null --podman-run-args "…" run --rm --no-deps --name agent-run-<agent>-<pid>-<random> …`
   as a background child, using `<&0` to keep the terminal, and `wait` on it.
   The extra mounts are `<ws>/.git` and each existing read-only folder
   read-only, and `<tmp>/empty` read-only over each existing hidden folder; `--podman-run-args` carries the `/tmp`,
   `/var/tmp` and `/dev/shm` mounts. Release the memory lock once the
   container exists.
5. Signal traps (`HUP`, `INT`, `TERM`) stop and remove the container. Bash
   only runs them after a foreground step finishes, so each step before the
   launch is followed by `abort_if_signalled`; and if a signal arrives before
   Compose has created the container, the wait loop keeps stopping it until
   Compose exits. Then
   make sure it's gone, run the memory quarantine (`after`), print the report,
   and exit with the container's status, or 128+N after a signal.

## Invariants: do not regress these

- Agents never see or change an existing `<workspace>/.signing`, and never
  change an existing `<workspace>/.idea`, including through hard links.
- Host Git never loads hooks or included configuration from agent-writable
  files, in the workspace repository or the memory repository. For settings
  that run commands this is best effort (path-like words only); don't widen
  that claim.
- Containers always run rootless, inside a user namespace. Agents and proxies
  both run with a read-only root filesystem, no capabilities,
  `no-new-privileges` and resource limits.
- Agents never get write access to `<workspace>/.git`, `bin/agent`,
  `compose.yaml`, `proxy/`, or any host path outside the declared mounts.
- No writable mount overlaps another mount or the launcher root. Compare
  canonical paths (`cd -P … && pwd -P`), never names.
- `bin/agent` sources libraries only from its resolved launcher root.
- Memory's `.git` is trusted only when it matches the record made by
  `agent trust-memory`; never infer trust from a `.git` merely existing.
- The memory quarantine never moves files on the host; it runs in its
  sandbox container, with every other container that has memory mounted
  paused, under the memory lock.
- The proxy allowlist never matches by reverse DNS (`dstdomain -n`).
- Engram's repository `.git`, `engram` entry point and engram-core scripts
  are never mounted; only the project's package (read-only), its `.memory`
  and engram-core's skills (read-only).
- Workspace files can't influence the launch. Keep `--env-file /dev/null` and
  the `cd` to the launcher root.
- The host never runs `git` in an agent-writable repository, and the report
  uses `find -P`. If you add host-side Git operations, they must target only
  the protected repository, or run sandboxed.
- No secrets in images, compose files, knowledge or memory.
- Everything stays pinned: images by digest, npm packages by lockfile. npm
  install scripts never run, at build time or when updating lockfiles.
- Allowlists stay minimal. Add a host only when an agent demonstrably needs
  it, and prefer exact hosts over `.domain` wildcards.

## Verified tool quirks (podman 6.1.4, podman-compose 1.6.0, macOS)

- By default podman-compose loads `.env` from the compose file's directory
  **and from the current directory**. A workspace `.env` containing
  `WORKSPACE=/etc` changed the mounts. `--env-file /dev/null` turns off both.
- A `- /tmp` entry in `compose.yaml` becomes a **named** volume
  (`agent-run_<service>_<hash>`). It survives `--rm` and is shared across runs.
  That's why `/tmp` is passed via `--podman-run-args "--mount type=volume,dst=/tmp"`.
- With `--userns keep-id`, Podman's automatic read-only tmpfs gets mounted
  **over** a `/tmp` volume. Hence `--read-only-tmpfs=false`, plus an explicit
  `/dev/shm` tmpfs.
- **Rootful** Podman (`rootful=true` machine or `-root` connection) ignores
  `--userns keep-id`: `/proc/self/uid_map` is `0 0 4294967295`, so there's no
  user namespace at all. Rootful also exposes the VM's sshd on each network's
  gateway (e.g. `10.89.1.1:22`); rootless (pasta) doesn't, and only container
  DNS (port 53, internal names only) answers there. Verified rootless mapping:
  container `node` (1000) is VM user 501 (`core`), container root an
  unprivileged subordinate UID.
- `git config --show-origin` prints the repository's own config as the
  relative `file:.git/config`; resolve it against the workspace. A config file
  containing just `x` parses fine (exit 1, no match); real syntax errors exit 128.
- npm install scripts never run (`--ignore-scripts` for every agent). Claude
  Code (2.1.301) still depends on its postinstall to move the platform binary
  from `node_modules/@anthropic-ai/claude-code-linux-<arch>/claude` over the
  `bin/claude.exe` placeholder; the `Dockerfile` does that move itself as a
  workaround. Drop it once Claude Code installs correctly with
  `--ignore-scripts`; check with `claude --version` in a fresh build.
- `ubuntu/squid` tags are `<squid>-<ubuntu>_<channel>`, and only `edge` and
  `beta` exist; interim Ubuntu releases (24.10, 25.04) stop getting updates,
  so stay on an LTS one. The current `7.2-26.04_edge` is a chiselled rock:
  `pebble enter` starts `entrypoint.sh` → `/usr/sbin/squid-gnutls` with the
  mounted `/etc/squid/squid.conf`, as UID 584792 (`_daemon_`), with no shell
  tools beyond `sh`. pebble only forwards Squid's output (our access log)
  with `--verbose`, hence `CMD ["--verbose"]` in `proxy/Dockerfile`; without
  it `podman compose logs proxy-<agent>` stays empty. `squid -k parse` warns
  "HTTP requires the use of Via" because of `via off`; that's expected.
- The proxy image is pinned in `proxy/Dockerfile`, not `compose.yaml`:
  Dependabot's Docker support reads Dockerfiles, while it's unclear whether
  its docker-compose support finds a file named `compose.yaml`.
- Squid's `dstdomain` matches a bare-IP `CONNECT` against the PTR name
  unless the ACL has `-n`. Verified: with `one.one.one.one` allowed,
  `CONNECT 1.1.1.1:443` was tunnelled without `-n` and refused with it.
- `resolve_path` re-resolves the whole path after every symlink (a link to a
  symlinked or not-yet-existing location must still come out canonical) and
  fails after 40 hops instead of looping.
- `podman pause` works rootless here (cgroup v2): a paused container makes no
  writes until `podman unpause`. `podman events` lags slightly; query it a
  moment later with an `--until` in the future.
- A missing bind-mount source is an error, so conditional mounts belong in
  `bin/agent`, not in `compose.yaml`.
- Bash defers traps while a foreground child runs. Compose must stay a
  background child that the script `wait`s on.
- `podman compose` prints `Error: executing … exit status N` whenever the
  container exits nonzero. It's harmless noise.

## Conventions

- `bin/agent`, `lib/` and the tests must run on macOS `/bin/bash` 3.2: no
  associative arrays, no `mapfile`, no `${var,,}`, no `local -n`. Use
  `set -euo pipefail` (in `bin/agent` and the test runners; libraries set no
  options). Capture expected failures with `|| status=$?`.
- Libraries are sourced only from the resolved launcher root, never from the
  workspace, `PATH` or a relative path. They define functions and constants
  only, with no top-level actions.
- Library functions take their inputs as arguments. Bash 3.2 can't return
  arrays, so a function that produces a list sets a documented output
  variable (`MEM_REPO`, `MEM_PROTECTED`, `CONTAINER_NAME`); the git guard
  keeps its context from `git_guard_init`. Never call a function that may
  `die` inside `$(…)`: `die` would only exit the subshell.
- Pass canonical paths (`canonical`) into the libraries; they compare paths
  as strings. On macOS the temp folder is behind a symlink
  (`/var` → `/private/var`), which is why `tests/run.sh` canonicalizes its
  scratch folder.
- Every behavior change gets a unit test, or an integration check if it
  needs Podman.
- Handle paths NUL-delimited (`find -print0`, `read -r -d ''`) and print them
  with `printf '%q'`.
- Match the existing style: short comments that explain *why*, plain wording,
  README tables for mounts and sharing.
- One agent per image and per service, with no shared binaries or config
  between agents. Adding an agent touches `agents/<agent>/` (package files
  and lockfile, made with `npm install --package-lock-only`), `compose.yaml`
  (service, proxy and network), `proxy/allowlists/<agent>.txt`, the `case` in
  `bin/agent`, `.github/dependabot.yml` (its npm `directories`), and the
  README.

## Testing

```sh
tests/run.sh                 # unit tests for lib/, seconds, no Podman
tests/run.sh git_guard       # one suite (tests/unit/git_guard_test.sh)
tests/integration.sh         # against the Podman machine; needs the claude image
```

- **Unit tests** (`tests/unit/<lib>_test.sh`): every `test_*` function runs in
  its own subshell, with a fresh scratch folder `$T` (the working directory),
  a scratch `HOME`, and `GIT_CONFIG_GLOBAL=/dev/null` plus
  `GIT_CONFIG_NOSYSTEM=1`, so the user's Git config never leaks in. Helpers
  are in `tests/helpers.sh`: `assert_ok`, `assert_dies TEXT CMD…`,
  `assert_eq`, `assert_contains`, `new_repo`, and `g` (Git without hooks or
  signing). They cover the refusal matrix, the Git guard, hard links,
  quarantine (including hostile names and memory as a repository) and the
  report.
- **Integration checks** (`tests/integration.sh`): read-only `.git`, `.idea`
  and hidden `.signing`; user namespace and capabilities; disk-backed `/tmp`;
  the network policy, including a bare IP whose PTR names an allowed host
  (a private test proxy); memory as a repository, an unrecorded memory `.git`
  and a read-only folder in quarantine; the hooks and rootful refusals;
  exit-code passthrough; `TERM` before and after the container exists; no
  leftover containers or volumes; engram (links resolve, package read-only,
  `.memory` writable and quarantined, `.git` and scripts absent, report,
  `AGENT_ENGRAM=0`). They use scratch repositories and a scratch `HOME`, with
  `CONTAINER_HOST` and `CONTAINER_SSHKEY` taken from the
  `podman-machine-default` connection.
- **Check that a new test can fail.** Break the code it covers, run the
  suite, and restore the code; every check should be caught by at least one
  test. A background job started through a shell function is a subshell, so
  `$!` is not the wrapper's PID; start `bin/agent` directly when signalling it.
- **By hand, in throwaway locations only:** never use the user's real
  repositories or `~/.agent-run`. Use a fresh scratch path per Podman run:
  deleting and recreating the same path within seconds can leave the VM's
  file share stale (`statfs … no such file or directory`). Without a TTY, set
  `GIT_PAGER=cat` in the container, or `git diff` blocks in a pager. The
  container reads the wrapper's stdin.
- `shellcheck` isn't installed here; run `bash -n` on `bin/agent` and
  `lib/*.sh` at least.

## Git hygiene

- Don't commit unless asked.
- Never add generated files, test fixtures or anything under `workspace/`
  except `.gitkeep`.
