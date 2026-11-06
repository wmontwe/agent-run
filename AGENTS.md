# AGENTS.md

Guidance for agents working on this repository. User documentation is
[README.md](README.md); keep both consistent when behavior changes.

## Purpose

agent-run runs coding agents (Claude Code, Codex, Gemini CLI, Pi) in rootless
Podman containers on macOS, isolated from the host and from each other.
Threat model: **the agent in the container is untrusted.** Everything here
limits what it can reach and what it can leave behind for the host to run.

## Layout

| Path | Role |
| --- | --- |
| `bin/agent` | The launcher and only supported entry point. Holds the configuration (`hidden`, `readonly_dirs`, mount sources) and the order of the checks. |
| `lib/common.sh` | Errors and path helpers (`die`, `canonical`, `overlaps`, `resolve_path`). |
| `lib/protect.sh` | `.git` and protected-folder checks, memory repository detection, hard links. |
| `lib/git-guard.sh` | Git config guard. |
| `lib/memory.sh` | Memory quarantine. |
| `lib/lock.sh` | The memory lock. |
| `lib/engram.sh` | Engram detection and validation. |
| `lib/tools.sh` | Build tools: the install container and the agent's tool mounts. |
| `lib/github.sh` | Checks the GitHub relay's repository list. |
| `lib/report.sh` | Snapshot and post-run report. |
| `lib/podman.sh` | Rootless check, `compose`, container naming and cleanup. The only library that calls Podman. |
| `tests/` | `run.sh` and `unit/` (no Podman), `integration.sh` (needs the Podman machine). |
| `compose.yaml` | One agent service and one Squid proxy per agent, plus `proxy-tools`. |
| `Dockerfile` | One image per agent (`ARG AGENT`) with only that CLI. |
| `agents/<agent>/` | `package.json`, `package-lock.json` and `.npmrc` pinning the agent's CLI. |
| `proxy/` | Squid image, shared `squid.conf`, and `allowlists/`: `<agent>.txt` per agent, `build.txt` for every agent, `tools.txt` for `proxy-tools` only, `none.txt` empty. `github-relay/` holds the relay's nginx image and config. |
| `workspace/` | Default workspace for plain `podman compose run`. |

Host state is under `~/.agent-run/`: `knowledge/` (read-only `/knowledge`),
`memory/` (read-write `/memory`, shared by all agents), `<agent>/home/`
(`/home/node`), `quarantine/` (never mounted), and `github-repos.txt` (the
relay's repositories, never mounted in an agent).

## Architecture

- **Workspace path:** the workspace is mounted at its canonical host path
  (and is the working directory), so absolute paths in build state and
  relative links, engram's included, mean the same on the host and in the
  container. Paths under the container's own top-level folders
  (`CONTAINER_RESERVED`) are refused.
- **Containers:** read-only root, no capabilities, `no-new-privileges`,
  `userns_mode: keep-id:uid=1000,gid=1000`, resource limits, no engine
  socket. Base and proxy images pinned by digest, CLIs by lockfile and
  installed with `npm ci --ignore-scripts`.
- **Egress:** each agent sits on its own `internal: true` network; its only
  way out is its proxy (`proxy:3128`), which allows HTTPS `CONNECT` to
  listed hosts. The proxy's `egress` network can reach the Mac, the VM's
  sshd and the gvproxy gateway; Squid's `private` ACL denies those.
- **GitHub relay:** a CONNECT proxy can't see methods or paths, so GitHub
  stays off the allowlists. `github-relay` (nginx, on every agent's network
  and `egress`) ends the agent's connection and forwards only GET and HEAD
  for the repositories in `~/.agent-run/github-repos.txt`, to `api.github.com`
  only, with a verified certificate and without the agent's headers. It adds
  no token, so GitHub's anonymous rate limit applies. `bin/agent` starts it
  only if that file exists and refuses a list with invalid lines.
- **Git:** `<workspace>/.git` is mounted read-only; agents never commit. Git
  settings that make host Git load files from an agent-writable folder are
  refused: enforced for hooks and includes, best effort for settings that run
  commands (only path-like words are resolved).
- **Protected paths** (top level only): `.signing` is covered by an empty
  read-only folder and `local.properties` (host paths, often secrets) by an
  empty read-only file; `.idea` is mounted read-only and refused if it
  contains a symlink. Missing ones are not created; a symlink or the wrong
  kind of entry in their place is refused.
- **Hard links:** protected files that share an inode with a file in a
  writable mount are refused before launch. Agents can't create such links,
  since `link()` across mounts fails.
- **Memory:** before and after each run, everything that isn't a directory or
  a regular `.md`, `.jsonl` or empty `.gitkeep` file, or that has an unsafe
  name, is moved to quarantine. The quarantine runs in a sandbox container,
  never on the host, because memory is shared and a concurrent agent could
  swap in symlinks; meanwhile every other container with memory mounted is
  paused, under `~/.agent-run/memory.lock`. Memory may be a Git repository:
  its `.git` (and an existing `.gitignore`/`.gitattributes`) is mounted
  read-only, but only when it matches the record written by
  `agent trust-memory`, since an agent can create a `.git` itself.
- **Engram:** top-level workspace links of exactly the form
  `../<repo>/projects/<project>/<name>` (named like their target, all into
  one project) mount that project's package read-only at the same relative
  path, its `.memory` writable (treated like memory: guard, hard links,
  pause, quarantine), and engram-core's skills read-only. A project's own
  `AGENTS.md` file is covered read-only by the package's. The repository's
  `.git`, `engram` and engram-core scripts are never mounted. Links of that
  form that disagree or don't validate are refused. `AGENT_ENGRAM=0` turns it
  off.
- **Build tools:** the project configures its runtimes and build (mise,
  Gradle, Kotlin, JVM settings); agent-run only provides them. Before the
  agent starts, an install container (agent image, read-only, no
  capabilities, network only through `proxy-tools`) gets copies of the
  project's mise files and Gradle wrapper, never the workspace. It runs
  `mise install --locked`, lets the project's wrapper download its
  distribution (the jar runs only if it matches Gradle's published checksum
  for that version; the wrapper checks `distributionSha256Sum`), and copies
  the host's Android SDK licenses. Results live in per-agent volumes: `/mise`
  (read-only for the agent), `/android` and `~/.gradle`. Every JVM gets the
  proxy through `JAVA_TOOL_OPTIONS`. On ARM64 the image has x86-64 runtime
  libraries, because Android's `aapt2` exists only for x86-64 and runs
  through the VM's QEMU.
- **Memory limit:** `AGENT_MEMORY` sets the container limit and nothing else;
  `bin/agent` prints it at launch.
- **Global gitignore:** the host's global ignore file (`core.excludesFile`,
  else Git's default) is copied into the temp folder and mounted read-only,
  with `GIT_CONFIG_*` pointing the container's Git at it.
- **Report:** after the run, an advisory report (`find -P`, not Git, so
  `.gitignore` can't hide anything) lists nested `.git` entries and created,
  changed or deleted files that host tools load.

### `bin/agent` flow

1. **Preflight:** resolve the launcher root, canonicalize the workspace,
   require a real `.git` directory and a rootless connection, check the
   protected folders, refuse overlapping mounts.
2. Create the temp folder (it must not overlap any mount) and run the Git
   guard, which fails closed if Git can't read its config.
3. Install build tools under a per-agent lock.
4. Take the memory lock and quarantine memory.
5. From the launcher root, start the proxy, snapshot the workspace, and run
   Compose as a background child; release the memory lock once the container
   exists.
6. On exit or signal: stop the container, quarantine memory, print the
   report, exit with the container's status (or 128+N).

## Invariants: do not regress these

- Agents never see or change an existing `.signing` or `local.properties`,
  and never change an existing `.idea`, including through hard links.
- Host Git never loads hooks or includes from agent-writable files, in the
  workspace or the memory repository. For command settings this is best
  effort; don't widen that claim.
- Containers always run rootless in a user namespace, with a read-only root,
  no capabilities, `no-new-privileges` and resource limits.
- Agents never get write access to `.git`, `bin/agent`, `compose.yaml`,
  `proxy/` or any host path outside the declared mounts.
- No writable mount overlaps another mount or the launcher root. Compare
  canonical paths, never names.
- Libraries are sourced only from the resolved launcher root.
- Memory's `.git` is trusted only when it matches the `trust-memory` record.
- The quarantine never moves files on the host.
- Allowlists never match by reverse DNS (`dstdomain -n`), stay minimal, and
  prefer exact hosts. Agents never reach `proxy-tools` hosts (GitHub), and
  reach `api.github.com` only through the relay.
- The relay never forwards anything but GET and HEAD, anywhere but
  `api.github.com`, and never turns off certificate verification.
- Tools are installed only from a lockfile or checksum; the tool volume is
  read-only for agents.
- agent-run never configures the project's runtimes or build settings.
- Workspace files can't influence the launch: keep `--env-file /dev/null` and
  the `cd` to the launcher root.
- The host never runs `git` in an agent-writable repository.
- No secrets in images, compose files, knowledge or memory.
- Everything is pinned; npm install scripts never run.

## Tool behavior the code relies on

- podman-compose loads `.env` from the compose file's directory and the
  current directory; `--env-file /dev/null` disables both.
- podman-compose turns `- /tmp` into a named volume that survives `--rm`, and
  prefixes named volumes given to `run -v` with the project name. Volumes
  therefore go through `--podman-run-args --mount …`.
- `--podman-run-args` also applies to dependencies, so the proxy is started
  separately and the agent runs with `--no-deps`.
- With `--userns keep-id`, Podman's read-only tmpfs covers a `/tmp` volume;
  hence `--read-only-tmpfs=false` and an explicit `/dev/shm`.
- Rootful Podman ignores `--userns keep-id`, which is why rootful is refused.
- A missing bind-mount source is an error, so conditional mounts are added in
  `bin/agent`, not `compose.yaml`.
- Bash defers traps while a foreground child runs: Compose runs in the
  background and the script `wait`s on it. `podman compose` prints
  `Error: executing … exit status N` for any nonzero exit; it's noise.
- `git config --show-origin` reports the repository's config as the relative
  `file:.git/config`.
- Squid's `dstdomain` matches a bare-IP `CONNECT` against its PTR name unless
  the ACL has `-n`.
- The Squid image (`ubuntu/squid`, LTS-based tags only) starts Squid through
  pebble, which only forwards Squid's log with `--verbose`.
- Squid's GnuTLS build (`ubuntu/squid`) doesn't verify an origin's certificate
  and can't read from GitHub over TLS 1.3, which is why the relay is nginx.
- nginx treats `\\` in a quoted string as one backslash; the relay's regex uses
  `\x5c`. It also needs its temp paths under `/tmp` on a read-only root.
- Claude Code needs its postinstall script to put its binary in place; the
  `Dockerfile` does that step itself. Remove it once Claude Code works with
  `--ignore-scripts`.
- Java ignores `HTTPS_PROXY`; `JAVA_TOOL_OPTIONS` reaches every JVM,
  including Gradle's daemons and test workers. Each JVM prints a
  "Picked up JAVA_TOOL_OPTIONS" line on stderr.
- Gradle distributions on `services.gradle.org` redirect to GitHub, which
  agents can't reach, so the install container downloads them. Gradle
  publishes wrapper jar checksums (on `downloads.gradle.org`) but not the jars.

## Conventions

- Bash 3.2 (macOS `/bin/bash`): no associative arrays, `mapfile`, `${var,,}`
  or `local -n`. `set -euo pipefail` in `bin/agent` and test runners only.
- Libraries define functions and constants only. Functions take inputs as
  arguments; a function producing a list sets a documented output variable.
  Never call a function that may `die` inside `$(…)`.
- Pass canonical paths into libraries; they compare paths as strings.
- Handle paths NUL-delimited and print them with `printf '%q'`.
- Every behavior change gets a unit test, or an integration check if it needs
  Podman.
- Short comments that explain *why*. No derived or guessed settings: leave
  configuration to the tool that owns it.
- One agent per image and service. Adding an agent touches `agents/<agent>/`,
  `compose.yaml`, `proxy/allowlists/<agent>.txt`, the `case` in `bin/agent`,
  `.github/dependabot.yml` and the README.

## Testing

```sh
tests/run.sh                 # unit tests, no Podman
tests/run.sh git_guard       # one suite
tests/integration.sh         # against the Podman machine; needs the claude image
```

- Unit tests run each `test_*` in a subshell with a scratch folder `$T`, a
  scratch `HOME` and no user Git config. Helpers are in `tests/helpers.sh`.
- Integration checks use scratch repositories and a scratch `HOME`, and the
  real `agent-run-claude-*` tool volumes.
- Check that a new test can fail: break the code, run it, restore the code.
- Try things by hand only in throwaway locations, never in the user's
  repositories or `~/.agent-run`, and use a fresh path per Podman run (a
  recreated path can leave the VM's file share stale).
- `shellcheck` isn't available; run `bash -n` on `bin/agent` and `lib/*.sh`.

## Git hygiene

- Don't commit unless asked.
- Never add generated files, test fixtures, or anything under `workspace/`
  except `.gitkeep`.
