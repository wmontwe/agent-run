---
name: review-agent-run-security
description: Review agent-run security when explicitly requested (security review, threat model, isolation audit), including its launcher, mount plan and gate, protected folders, host-tool checks, egress proxy, supply chain, and isolation boundaries.
---

# Review agent-run security

Read README.md, AGENTS.md and docs/architecture.md first: the threat model, accepted
risks, invariants and protection tests are the guarantees to check. Inspect `crates/*`
(the launcher in `crates/agent`, the libraries it orchestrates, the gate), `bin/agent`,
`image/Containerfile`, `agents/*/` (package.json, package-lock.json, .npmrc),
`proxy/Dockerfile`, `Cargo.lock` and `.github/dependabot.yml`.

Treat agents as arbitrary-code adversaries, and anything they can write as hostile.
Treat documentation and recorded results as claims to verify.

Review without modifying project files or committing. Recommend minimal fixes consistent
with the design.

## Review boundaries

- Launch integrity: the policy read only from the host and validated strictly; canonical
  paths; symlinks in any mount target's path; characters Podman's arguments can't carry;
  how the launcher builds Podman arguments.
- Isolation mode: rootless Podman refused otherwise; the pod's user namespace
  (`keep-id`); read-only root, no capabilities, `no-new-privileges`, resource limits.
- Mount enforcement: the mount plan (masks, read-only folders, pinned parents, private
  volume subpaths, the host's Gradle state); the gate holding every container until its
  mounts match the plan by type-specific identity, inode included; races between planning
  and start; case aliases on a case-insensitive checkout; warm starts reusing a pod's plan,
  the rules hash, and folders reported as unprotected.
- Host tools: the IntelliJ reload check; engram's locked links and the immutable flag; the
  Git config guard (host Git loading hooks or includes from agent-writable files), which
  the design requires at start; nested `.git` entries and hard links, accepted risks.
- Egress: one proxy, per-pod rules by subnet, names refused before resolving (`-n`),
  private and VM/host destinations, `CONNECT` only, live reloads, the proxy's hardening,
  allowlist breadth (hosts usable for exfiltration).
- Lifecycle: the start lock, unique names, attaching and detaching, signals, exit status,
  cleanup of containers, idle pods and proxy rules.
- Supply chain: image digests (base, Rust, Squid), lockfiles, npm install scripts never
  running, the Claude binary workaround, Cargo dependencies, self-updaters, and what
  Dependabot can and cannot update.
- Documentation: enforced guarantees versus accepted risks; claims the tests don't cover.

## Verify safely

Use disposable checkouts and scratch state. Never use personal repositories, real
credentials, or the real `~/.agent-run`: point `AGENT_RUN_HOME` at a scratch folder with
its own `policy.toml` (keep `HOME`, so Podman finds its connection).

Use harmless canaries. Never execute planted hooks on the host. Create scratch Git
fixtures with `git -c core.hooksPath=/dev/null -c commit.gpgsign=false`.

Building images, starting containers and creating volumes is allowed. Record `podman ps
-a`, `podman volume ls` and `podman network ls` first and leave them as you found them;
never stop or reconfigure the Podman machine. A proxy container started with scratch state
must be removed afterwards: the real launcher reuses it.

Follow the project's testing guidance (AGENTS.md): `cargo test`, the `--ignored` Podman
tests, `cargo clippy`. For a new attack, prefer adding a failing test over a one-off
reproduction.

Record the actual OS, Podman backend and version, rootless mode and network backend. Do
not generalize native Linux results to macOS Podman machines, or rootful results to
rootless.

If implementation or runtime access is missing, continue the available review and mark the
remaining checks unverified.

## Report

Rate severity against the guarantees:

- Critical: host code execution, or writes to protected folders or Git metadata.
- High: missing isolation (no user namespace, reachable privileged host services), reading
  masked folders, or escaping the egress proxy.
- Medium: bypassing an enforced check, or confidentiality leaks with preconditions.
- Low: denial of service, or documentation that overstates a guarantee.

Lead with findings ordered by severity. Include for each: file and line; preconditions and
reproduction or concrete static evidence; impact and the affected guarantee; the smallest
practical fix.

Separate demonstrated vulnerabilities, unverified concerns, and accepted risks. List tested
environments and verification gaps. If nothing is found, say so without claiming untested
boundaries are proven safe.
