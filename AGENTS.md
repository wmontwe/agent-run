# AGENTS.md

Guidance for agents working on this repository. User documentation is
[README.md](README.md) and [Configuration](docs/configuration.md). Component responsibilities,
trust boundaries and invariants are in [Architecture](docs/architecture.md).
Keep these consistent when behavior changes.

## Purpose

agent-run runs coding agents in rootless Podman containers on macOS, on the user's live
checkout. Threat model: **the agent in the container is untrusted**, and so is anything
it can write. agent-run prevents; it doesn't report after the fact.

## Layout

| Path | Role |
|---|---|
| `crates/policy` | Reads and validates `~/.agent-run/policy.toml` |
| `crates/workspace` | Inspects a checkout: rule folders, settings roots, the host's Gradle state; the IntelliJ, Git config and engram checks |
| `crates/mountplan` | The mount plan, its Podman arguments and hash, and the verification of a gated container |
| `crates/gate` | The container entrypoint that holds the command until the mounts are verified (Linux, static) |
| `crates/podman` | The only code that runs Podman; the shared-service type and the namespace of all names |
| `crates/pod` | A project pod's lifecycle, its names and the plan it started with |
| `crates/egress` | The egress proxy's configuration, and the denials in its log |
| `crates/approvals` | Approved destinations: what `allow` and `revoke` change, session approvals, policy edits |
| `crates/relay` | The GitHub relay's repository map |
| `crates/mirror` | The dependency mirror's configuration and allowlist |
| `crates/agents` | Each agent CLI's conventions: its command line with the session's MCP servers, its global instructions, its files |
| `crates/sessions` | An agent session's container: host mounts, environment, Podman arguments; knows no CLI |
| `crates/builds` | The build container: volumes, settings, Podman arguments |
| `crates/vision` | A session's display and browser in the build container, and its MCP entry |
| `crates/engram` | A project's engram service: its container, socket, the agent's read-only mounts, the save on the host |
| `crates/buildrun` | `gradle-run` and its server in the build container (Linux, static) |
| `crates/agent` | The launcher: runs the steps in order, prints progress; `services.rs` wires the shared services |
| `bin/agent` | Builds and runs the launcher |
| `image/` | The agent image (`Containerfile`, built with the gate and `gradle-run`) |
| `build-image/` | The build image: JDKs, the gate, `gradle-run-server`, the init scripts, each session's display and browser (`agent-run-display`, `browser-mcp`) |
| `agents/<agent>/` | The agent CLI's pinned `package.json` and `package-lock.json` |
| `proxy/` | The Squid image; `github-relay/` the relay's, `mirror/` the mirror's |
| `docs/` | Architecture, threat model and security guarantees |
| `.github/` | Dependabot's configuration and CI |

## Conventions

- One crate per problem, with one job and a narrow interface; the launcher only
  orchestrates and holds no problem-specific logic.
- No async runtime, the `podman` CLI rather than an API client, and few dependencies,
  each with a reason.
- Errors are messages for the user: name the path, key or value and what to do.
- Paths compare as canonical paths, never by name. Paths Podman's arguments can't carry
  safely (`,`, `:`, tabs, newlines) are refused, not escaped.
- Mounts protect folders only: on a case-insensitive checkout, a mount over a single file
  is bypassed through another spelling.
- Write files the VM shares in place; a file renamed over another shows as missing in
  containers for seconds.
- Shared code is extracted, not copied: a new service is its crate plus one entry in
  `crates/agent/src/services.rs`; Podman object names come from the namespace.
- Short comments that explain why.
- Formatted with `cargo fmt --all` (rustfmt's defaults) before every change is done.

## Testing

```sh
cargo test                                  # unit tests, no Podman
cargo test -p podman -p mountplan -p pod -p egress -p relay -p mirror -p vision -p builds -p engram -p agent -- --ignored   # local release gate; fixtures required below
cargo clippy --all-targets
cargo fmt --all --check                     # code is formatted with rustfmt's defaults
```

CI (`.github/workflows/ci.yml`) runs formatting, clippy and the unit tests on macOS, checks
the oldest supported Rust (`rust-version`), and builds every image on arm64 Linux. Image
smoke tests exercise the gate hold/release protocol, agent CLI startup, Java majors,
Playwright/Chromium rendering and infrastructure runtime binaries.

### Local release gate

Before accepting a release or dependency update, run the ignored Podman suites above on
macOS Apple Silicon with rootless krunkit/libkrun. Use disposable checkouts, scratch state
and isolated namespaces, never personal credentials or the real `~/.agent-run`. Explicitly
set `AGENT_RUN_TEST_ENGRAM_CORE` to a disposable, built fixture and
`AGENT_RUN_TEST_WRAPPER_FROM` to a disposable checkout containing a Gradle wrapper before
running; do not use the engram tests' personal-directory defaults.

Record the tested revision, OS/Podman/provider, exact commands and results. Include mount
and case-alias protection, authenticated egress/revocation, lifecycle/rollback, multi-project
service recovery, builds, licenses and resource limits. Record missing fixtures and skipped
suites as verification gaps.

### Test setup

- A crate's own behavior is tested in the crate (`crates/<crate>/tests`); `crates/agent/tests`
  holds only what needs a whole session: the protections, the commands, and how the crates
  work together.
- Podman tests are `#[ignore]` and need the Podman machine. They use
  `localhost/agent-run-gate` (`podman build -t localhost/agent-run-gate -f
  crates/gate/Containerfile --ignorefile crates/gate/Containerfile.ignore .`), an agent
  image (`AGENT_RUN_TEST_IMAGE`, default `localhost/agent-run-claude`), and for egress the
  internet. The relay's test needs `localhost/agent-run-github-relay` (`podman build -t
  localhost/agent-run-github-relay proxy/github-relay`), the mirror's
  `localhost/agent-run-mirror` (`proxy/mirror`). The agent's suites start real sessions with
  `tests/common` (a scratch home and namespace, removed afterwards) and build the images if
  they're missing; `builds` also needs `AGENT_RUN_TEST_WRAPPER_FROM`, a checkout to copy a
  Gradle wrapper from, and `agents` builds the Codex, Gemini and Pi images; the engram tests
  copy engram-core with its built server; `cli` runs the public `agent` command, `rollback`
  injects failures after each start step (`AGENT_RUN_FAIL_AFTER`, in that test binary only)
  (`AGENT_RUN_TEST_ENGRAM_CORE`). `podman`, `vision` and `engram` use the build image.
- Check that a new test can fail: break the code, run it, restore the code.
- Try the launcher only on throwaway checkouts, with `AGENT_RUN_HOME` pointing at a scratch
  folder instead of `~/.agent-run`.

## Git hygiene

- Don't commit unless asked.
- Never add generated files or test fixtures.
