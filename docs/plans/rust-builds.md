# Rust build support: implementation and verification record

Status: implemented in the working tree; self-hosted buildrun/policy/egress tests, Clippy
and formatting now pass through `cargo-run` with build-only network access. Full
macOS/Podman acceptance is pending. This precedes [Kotlin Toolchain integration](kotlin-toolchain.md).
No Oracle review has been performed.

## Implementation

- `crates/buildrun`: shared command client, typed Gradle/Cargo backend selection,
  backwards-compatible `gradle-run 1` and new `cargo-run 1` greetings. Both tools share
  canonical cwd validation, queue capacity, output framing, status and cancellation.
  Requests cannot name an arbitrary executable.
- `crates/buildrun/src/cargo.rs`: writable Cargo home and per-working-directory target
  outputs under `/build-state/rust`; read-only image toolchains under `/opt/rustup`.
  No host Cargo home, credentials or configuration are imported.
- `build-image/Containerfile`: versioned rustup bootstrap checked against its published
  SHA-256, Rust 1.99.1 with rustfmt/Clippy, native C/C++ build prerequisites, and zip/procps
  for runner tests. Existing pinned base-image references are unchanged.
- `image/Containerfile`: static `cargo-run` client, not compilers.
- `crates/workspace`: existing `target` folders are discovered without descending into
  them, including case variants and nested targets; unprotectable links/files are refused.
  Explicit policy rules remain authoritative. The mount plan pins target ancestors.
- `crates/builds`: instructions for all agents explain the bridge and Linux limitations.
- README, configuration, architecture and repository guidance describe these behaviors.

The first implementation supports image-installed Rust toolchains only. Project
`rust-toolchain.toml` and `+toolchain` selection remain rustup's job, but automatic installs
are disabled. Additional versions/components require an explicit image rebuild. Runtime
provisioning into writable per-project RUSTUP_HOME is a possible follow-up.

Default output redirection does not sandbox arbitrary build scripts or CLI overrides.
Existing host `target` folders are masked; custom output folders need workspace rules.
Source remains writable by design.

## Build-only network policy

The implementation now adds `[builds].allow` and per-checkout
`[projects."<path>".builds].allow`, merged without changing agents' grants. Egress uses
separate authenticated build identities rather than subnet-wide allowances. Credentials
are stored mode 0600 in trusted host state, keyed by the actual Podman pod generation,
reused for that generation and supplied only to its build container. Reused containers
with an older/missing identity require restart. Pod teardown/shutdown removes build state
credentials; a new pod ID cannot inherit a stale identity even after an interrupted cleanup.

Proxy reconciliation reloads current trusted policy so ending an older session cannot
restore its startup grants. Denial reports distinguish build identities and point to
`builds.allow`. This controls direct proxy requests only: agents can still delegate
arbitrary code to the build container. Authenticated proxy support is required in clients;
Gradle's existing mirror routing remains unchanged.

## Tests

- Protocol/backend selection, legacy Gradle greeting, unsupported versions, escaping,
  empty/Unicode arguments, and Cargo environment isolation.
- Real client/server process tests with a stand-in build: argv/cwd/display, status,
  outside-checkout refusal, shared queue and cancellation. Linux server fixtures become
  child subreapers before exec to match the production PID-1 server's reaping behavior.
- Bounded output forwarding yields to status/cancellation checks during continuous output.
- Workspace target discovery/case handling, explicit mask deduplication, link/file refusal,
  and mount-plan masks/ancestor pins.
- Image smoke tests: agent client without compilers, plus offline build-container
  compile/test/fmt/Clippy with a read-only root.
- Build policy parsing/merging and absence from agent grants; private credential
  persistence/validation, generation separation and cleanup; authenticated-only proxy
  rules. Ignored egress tests cover build-only access, unauthenticated/agent denial,
  cross-project credential use, and fresh-connection revocation.
- `crates/agent/tests/rust.rs`: ignored, disposable full-session acceptance test. Builds,
  formats and lints a dependency-free Rust project offline; checks result visibility and
  read-only access, root/nested target case aliases, and host sentinel preservation.
  Uses isolated policy/namespace, runtime-generated fixtures and bounded exec commands.

## Verification record

Base revision: `6d71fdf2d6a5c311e923acb9007836bb8615c5e7` plus the uncommitted changes.
No release revision/image digest has been captured. No commit was made.

### Before the session restart

Environment: Linux aarch64 agent container, Rust 1.99.1 bootstrapped under `/tmp` using
rustup 1.28.2 and its published SHA-256. Debian bookworm GCC 12.2, development files,
procps, zip and unzip were downloaded through authenticated Debian package indexes and
extracted under `/tmp/agent-run-native`, not installed on the host or in the checkout.
A temporary cc wrapper and relocated libc linker script supplied the native linker.
Cargo cache and target directories were also under `/tmp`.

With those tools on PATH and their extracted libraries on LD_LIBRARY_PATH:

- `cargo test --locked -p buildrun`: **11 unit tests and all 5 process tests passed**,
  including the real Gradle installer cancellation/retry test; no installer stub.
- `cargo clippy --locked -p buildrun --all-targets -- -D warnings`: **passed**.
- `cargo fmt --all` and `cargo fmt --all --check`: **passed**.
- Earlier dependency-free harness checks passed 8 unit/4 process tests and direct Clippy
  checks. Those were provisional; the complete Cargo results above supersede them.
- Three isolated source-copy mutation checks detected expected failures: Cargo greeting
  changed to Gradle, Cargo-home redirection removed, and output forwarding changed to
  drain-until-quiet. Repository source was not mutated for those checks.
- A temporary dependency-free project passed offline test/fmt/Clippy with toolchain files
  read-only and musl/rust-lld selected. This was not the formal GNU image smoke test.
- Static musl, MSRV and Darwin compile checks were started when the session crashed.
  Their outcomes were not collected; they remain unverified.

### First restart, before activating the build proxy identity

The temporary bootstrap directory is gone; use `cargo-run`, not another agent-side
compiler installation. The staged `crates/buildrun/Cargo.toml` change was preserved.

- `cargo-run --version`: Cargo 1.99.1 responds from the build container.
- `cargo-run fmt --all` and `cargo-run fmt --all --check`: **passed**.
- `git diff --check` and `sh -n .github/smoke-image.sh`: **passed**.
- `cargo-run --config net.retry=0 test --locked -p buildrun`: blocked fetching the
  crates.io index, proxy HTTP 403. The restarted build container lacks the shared network
  grants in the older running launcher; the earlier agent-scoped approvals do not
  authorize it. The new build-only identity/policy implementation supersedes the earlier
  suggestion to use shared grants and requires updating the launcher/restarting. Cargo Clippy has the
  same dependency-fetch blocker. This does not invalidate the earlier native test run,
  but it prevents claiming a successful self-hosted dependency build after restart.
- Build-only policy/identity implementation and added tests: formatted, but not compiled
  or run in this session; `cargo-run --config net.retry=0 test --locked -p policy -p egress
  -p buildrun` is still blocked by the older runtime's crates.io HTTP 403. Earlier passing
  tests predate these authorization changes and must not be presented as validating them.
- New ignored session acceptance test: formatted, not compiled/run yet. Podman is not
  exposed to the agent. macOS workspace/mount-plan tests and the formal image/release gate
  have not run. Availability of the upgraded clients is not a full image acceptance run.

### Restart with build-only policy active

Recorded 2026-11-15T21:40:10Z. Agent environment: Linux
`7.0.12-201.fc44.aarch64`, aarch64. Cargo 1.99.1 runs in the build container; the agent
has `/usr/local/bin/cargo-run` but no `cargo` or `rustc` executable. Host Podman version,
provider and image digests were not queried and remain release-verification gaps.

- `cargo-run --config net.retry=0 test --locked -p policy -p egress -p buildrun`:
  registry index/artifacts downloaded successfully; **39 tests passed** (11 buildrun unit,
  5 buildrun process, 10 egress unit, 13 policy unit). The real Podman egress test was
  **ignored**, not executed. The Gradle installer cancellation/retry test passed.
- `cargo-run clippy --locked -p policy -p egress -p buildrun --all-targets -- -D warnings`:
  **passed**, including compile-checking egress's ignored integration test.
- `cargo-run fmt --all --check`, `git diff --check` and
  `sh -n .github/smoke-image.sh`: **passed**.
- `curl -sS -I --max-time 15 https://index.crates.io/config.json` in the agent still
  receives proxy CONNECT **403**, while Cargo's build-container download succeeds. This
  is a live smoke check of the intended direct-access distinction, not a replacement for
  the isolated Podman authorization/revocation suite.
- `/build-state/rust/targets/debug` is visible and the target root is not agent-writable.
- Regression checks: temporarily removed global build-grant inheritance, then temporarily
  removed the build destination rule's authentication/identity predicates. Each targeted
  policy/egress test compiled and **failed as expected** (exit 101). Both files were restored
  immediately; the full 39-test command, Clippy and formatting then **passed again**.
  Existing staged source changes were preserved.

## Remaining acceptance work

1. On macOS, run full unit/lint/MSRV checks, especially workspace/mount-plan target tests.
   Linux Rust does not make the launcher's macOS APIs portable. Compile/run the new
   `cargo test -p agent --test rust -- --ignored` disposable-session acceptance test.
2. Run image smoke tests and the documented rootless Podman release gate on macOS Apple
   Silicon/krunkit. Include queue contention, cancellation, case aliases, result access,
   restart/reuse, stale images and cross-project isolation. Record image digests,
   OS/Podman/provider, exact commands/results, missing fixtures and skipped suites.
3. Resume Kotlin's upstream/output-isolation verification gate after the Rust bridge is
   accepted. Kotlin-owned JVM provisioning and Compose MCP are not implemented here.
