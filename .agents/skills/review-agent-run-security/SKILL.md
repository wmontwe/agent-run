---
name: review-agent-run-security
description: Review agent-run security when explicitly requested (security review, threat model, isolation audit), including its launcher, Podman mounts, Git protection, protected folders, memory quarantine, proxy, supply chain, and isolation boundaries.
---

# Review agent-run security

Read the current README.md and applicable AGENTS.md first.
Inspect bin/agent, lib/*.sh, compose.yaml, Dockerfile, agents/*/ (package.json,
package-lock.json, .npmrc), proxy/Dockerfile, proxy/squid.conf, the
per-agent allowlists, and .github/dependabot.yml.

Treat agents as arbitrary-code adversaries. Treat documentation
and recorded verification results as claims to verify.

Review without modifying project files or committing. Recommend
minimal fixes consistent with the project's existing design.

## Review boundaries

- Launch integrity: canonical paths, symlinks, overlap checks,
  argument handling, service validation, Compose environment loading
  (`--env-file /dev/null`, launch from the trusted root), and
  protection of launcher files.
- Isolation mode: the connection is rootless and refused otherwise;
  `/proc/self/uid_map` shows a real user namespace; capabilities,
  no_new_privs and seccomp.
- Mount enforcement: effective mounts, writable aliases, hard links,
  nested mounts, and races between validation and launch.
- Git protection: attempted writes, deletion, renaming, and replacement
  of .git; external metadata; the guard against `core.hooksPath` and
  `include.path` / `includeIf.*.path` pointing into the workspace
  (relative, `~`, `..`, symlinked and nested includes); command-valued
  settings that are not checked.
- Protected folders: `.signing` hidden (confidentiality and writes),
  `.idea` read-only (writes, symlinks inside it); alternate access
  paths such as committed copies in Git history, missing directories,
  and unsupported layouts.
- Memory quarantine: hostile filenames, symlinks, special files,
  destination collisions, concurrent replacement, and confinement of
  host-side traversal and movement. When memory is a Git repository: its
  read-only `.git`, `.gitignore` and `.gitattributes`, agent-made
  look-alikes, and Git config pointing between workspace and memory.
- Lifecycle: signals, exit status, shutdown before scanning, cleanup,
  unique container names, protected markers, and temporary-volume
  isolation.
- Resources: memory, CPU and process limits; unbounded `/tmp` volumes.
- Networking: direct egress, DNS, host/VM access (including services
  on the network gateway), private destinations, cross-agent access,
  proxy restrictions, allowlist breadth (hosts usable for exfiltration),
  and configuration integrity.
- Supply chain: image digests, lockfile integrity, npm install scripts
  never running, the Claude binary workaround, self-updaters, and what
  Dependabot can and cannot update.
- Reporting: no symlink following, NUL-delimited paths, escaped output,
  sensitive deletions, ignored files, nested Git entries, explicit
  traversal failures, and terminal escape sequences reaching the host
  (in the report, in tool error messages, and in the agent's own output).
- Documentation: distinguish enforced boundaries from advisory checks,
  shared-memory risks, and host execution of writable workspace content.

## Verify safely

Use disposable repositories and scratch state. Never use personal
repositories, real credentials, or the real ~/.agent-run.

`bin/agent` always touches `~/.agent-run` (memory quarantine, home
mount), so run it with a scratch `HOME` containing
`.agent-run/{knowledge,memory,<agent>/home}`. Podman then cannot find
its connection: set `CONTAINER_HOST` and `CONTAINER_SSHKEY` from the
rootless entry in `podman system connection list`. To test against the
user's global Git configuration, also set `XDG_CONFIG_HOME` to the real
one; it is only read, never written.

Use harmless canaries. Never execute planted hooks on the host.
Inspect source before running project commands. Create scratch Git
fixtures with `git -c core.hooksPath=/dev/null -c commit.gpgsign=false`.
Read Git configuration on the host only with `git --no-pager config
--file …` or `--no-includes`.

Building images, starting proxies and creating volumes is allowed.
Record `podman ps -a` and `podman volume ls` first and leave both as
you found them; never stop or reconfigure the Podman machine.

Follow project testing guidance: run tests/run.sh, and
tests/integration.sh when Podman is available; for a new attack,
prefer adding a failing test over a one-off reproduction. Check Bash syntax and Bash 3.2
compatibility where available. Use ShellCheck if installed.

Record the actual OS, Podman backend, rootless mode, network backend,
Compose provider, and versions. Do not generalize native Linux results
to macOS Podman Machine, or rootful results to rootless.

If implementation files or runtime access are missing, continue the
available review and explicitly mark remaining checks unverified.

## Report

Rate severity against the guarantees:

- Critical: host code execution, or writes to protected Git metadata.
- High: missing isolation (no user namespace, reachable privileged host
  services), reading hidden folders, or escaping the firewall.
- Medium: bypassing an enforced check, or confidentiality leaks with
  preconditions.
- Low: bypassing advisory checks, denial of service, or documentation
  that overstates a guarantee.

Lead with findings ordered by severity. Include for each:

- File and line reference.
- Preconditions and reproduction or concrete static evidence.
- Impact and the affected security guarantee.
- The smallest practical fix.

Separate demonstrated vulnerabilities, unverified concerns, and
accepted residual risks. List tested environments and verification gaps.
If no vulnerabilities are found, say so without claiming untested
boundaries are proven safe.
