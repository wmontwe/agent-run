# Kotlin Toolchain integration plan

Status: source contracts verified against the read-only upstream snapshot at
`7133c358a9605541d37d144d73f5860df5be6965` (2026-11-10, wrapper version
`0.14.0-dev-4624`). `kotlin-run` is now wired through the build protocol/server, packaged in agent images,
and included in all agents' instructions. Discovery/environment preparation lives in
`crates/buildrun/src/kotlin.rs`; Kotlin manifest roots get private build safety nets.
Cold provisioning/JVM proxy authentication remain runtime acceptance gaps, not a reason
to omit the build command. Compose MCP integration is still not enabled. Oracle review is pending;
no Oracle tool was available. The review below is local, not an Oracle result.

## Prerequisite: Rust build bridge

[Rust build support](rust-builds.md) now runs inside the build container: 39 portable
buildrun/policy/egress tests, Clippy and formatting pass through `cargo-run`. Registry
fetches work with build-only grants while direct agent requests remain denied. Regression
mutations for grant inheritance and authenticated proxy rules fail as expected. The
macOS-specific tests and full Podman acceptance still need execution; Linux checks do not
replace that release gate.

## Goal and scope

Expose `kotlin-run <arguments...>` in agent containers. Execute the project's Kotlin
Toolchain wrapper in the existing project build container, with the toolchain owning JVM
provisioning and selection, dependencies, plugins, and command semantics.

Initial acceptance target: Linux JVM projects, including project-defined plugin commands.
Do not claim Android, native, or Apple-target support from this milestone. Keep existing
Gradle behavior unchanged. No host execution, host JVM/cache import, global installation,
interactive stdin forwarding, or automatic toolchain upgrades.

Examples:

```sh
kotlin-run build
kotlin-run test
kotlin-run show commands
kotlin-run do updateDetektBaseline --module api --module core
```

Plugin commands use `do <name>`; they are not necessarily root subcommands. Forward all
arguments without an agent-run command allowlist. Toolchain/plugin validation belongs to
the project toolchain. Source-generating and baseline-updating commands may intentionally
edit the checkout, just like other untrusted build code.

## 1. Validate the upstream execution contract first

Before implementation, use a disposable project and a pinned upstream release to record:

- Wrapper discovery from root, module directories, and nested independent projects, using
  `project.yaml` / `module.yaml` and the local `kotlin` wrapper. Bound discovery to the
  canonical checkout and report missing wrappers; never fall back to a host/global tool.
- Wrapper runtime provisioning, distribution checksum verification, download destinations,
  proxy behavior, extraction, locking, cancellation, and retry after partial downloads.
- Supported output/cache redirection options from that release's CLI help and docs.
  Do not assume Gradle's output layout or invent a Kotlin output flag.
- A working local plugin with a command that writes a baseline and a task that generates
  sources; capture which paths are intended source edits versus disposable build state.
- Whether project initialization without a wrapper requires a separate future feature.
  Wrapper-based execution must not silently install a floating global version.

Revised after the user's project-session test clarified the deliverable: provide the actual
build bridge and instructions so an agent can attempt the build and report real failures.
The bridge is now enabled with verified command/protocol tests and output protection.
Do not claim a successful cold Kotlin/plugin build before real provisioning and JVM proxy
authentication pass acceptance; failures must remain visible and fail closed.

## 2. Separate shared execution from build-tool behavior

Owners: `crates/buildrun`, `crates/builds`; launcher remains orchestration only.

- Extract reusable request/client, queue, process execution, output streaming, display,
  status, and cancellation code from Gradle-specific behavior. Avoid copying a server or
  introducing a general remote-shell interface.
- The typed Gradle/Cargo selector now also includes Kotlin Toolchain (`kotlin-run 1`).
  Kotlin executes the discovered project wrapper directly with separate argv values and
  the caller's validated cwd. No shell interpolation or arbitrary executable request.
- Keep Gradle installation, options, init scripts, and environment in its backend.
- Define explicit wire-version handling. Preserve `gradle-run 1` if feasible; otherwise
  reject incompatible requests clearly and require restart of stale build containers.
  Never reinterpret an old Gradle request as Kotlin.
- Share the existing per-project concurrency budget across both tools. Toolchain upstream
  concurrency support does not remove agent-run's resource limit.
- Make diagnostics identify the correct client. Keep noninteractive execution explicit.

## 3. Let Kotlin Toolchain own the JVM and tooling

Owners: Kotlin backend in `crates/buildrun`, build-container setup in `crates/builds`.

- Set `KOTLIN_CLI_BOOTSTRAP_CACHE_DIR` and `KOTLIN_SHARED_CACHE_DIR` to distinct persistent,
  project-owned container storage, namespaced away from Gradle state.
- Set `KOTLIN_CLI_NO_WELCOME_BANNER=1` for predictable provisioning output.
- Require Kotlin Toolchain 0.14.0 or newer: it lets every download address be replaced.
  Point each (`KOTLIN_CLI_DOWNLOAD_ROOT`, `KOTLIN_TOOLCHAIN_*_URL`,
  `KOTLIN_DEFAULT_MAVEN_CENTRAL_URL`) at the mirror remote of its upstream, found by URL; the
  mirror serves HTTPS with agent-run's authority, since Kotlin Toolchain drops `http://`
  repositories. Set `KOTLIN_CLI_JAVA_HOME` and `JAVA_HOME` to the image's Temurin 25, always
  (a choice, not a fallback). Do not inject Gradle JDK paths, or Gradle init scripts/options.
- Remove inherited JVM overrides from Kotlin child processes. The current image sets
  `JAVA_HOME=/opt/jdk/21` and prepends that JDK to PATH: address both, plus relevant JVM
  option variables, rather than merely omitting an explicit JAVA_HOME assignment.
  Keep this sanitization backend-specific so Gradle remains unchanged.
- Verify the provisioned runtime and project JDK are actually used with cold caches.
  Never substitute an image JDK when provisioning fails.
- Do not forward arbitrary agent environment variables. Keep supported display/session
  metadata and required proxy settings explicit.

## 4. Isolate host state and expose build results

Owners: `crates/workspace`, `crates/mountplan`, `crates/builds`, `crates/pod` as needed.

- Discover Kotlin project roots and actual state folders using existing canonical path,
  case-alias, symlink, and mount-parent protection rules.
- Preserve pre-existing host Kotlin outputs/caches. Prefer verified toolchain redirection
  to a dedicated `/build-state/kotlin-toolchain` subtree. Retain private safety-net folder
  mounts for writes before redirection and for absent output directories.
- If upstream cannot redirect necessary outputs, design explicit private directory mounts
  and read-only result access for agents. Do not silently write host output folders or use
  single-file mounts. Resolve this before promising result paths.
- Keep Gradle and Kotlin output/cache namespaces distinct in mixed projects.
- Keep source/baseline edits available to plugin commands. Do not describe arbitrary
  plugin output paths as isolated: plugins can write anywhere the build can write.
- Maintain agents' read-only access to generated results. Document restart requirements
  for new protected folders and mount-plan/image changes.

## 5. Provision through controlled network access

Owners: `crates/mirror`, `proxy/mirror`, existing egress/policy components where necessary.

- Inventory distribution, bootstrap JRE, project JDK, Maven/plugin dependencies, and
  redirects from a clean-cache run. Approve only intended destinations in `builds.allow`,
  not the shared agent/build list. Verify upstream downloaders support the build proxy's
  authentication; forwarding proxy environment variables alone is not sufficient proof.
- Evaluate the documented `KOTLIN_CLI_DOWNLOAD_ROOT` for mirror routing; the default
  distribution root is `https://packages.jetbrains.team/maven/p/amper/amper`.
  Upstream labels this override unstable: test and document the release dependency.
- Preserve checksum verification. Do not turn the mirror into an arbitrary URL fetcher,
  bypass proxy policy, or treat project-declared repositories as trusted approvals.
- Verify Kotlin's own dependency/provisioning clients honor the selected proxy/mirror
  route; Gradle's mirror init script does not apply to Kotlin Toolchain.
- Failed/denied downloads must explain the destination and host-side remediation.
  Test interruption, retry, corrupt artifacts, and cache completion semantics.

## 6. Compose Hot Reload MCP integration

Status: planned follow-on milestone, not current support. The inspected upstream source
implements `./kotlin compose-hot-reload-mcp-server` as **stdio**, inherits stdin/stdout and
requires Compose Hot Reload **1.2.0-beta01 or newer**. It attaches to the running app via
`<build-dir>/hot-reload-app.pid`; app and MCP invocations must use the same build directory.
These are source contracts, not a completed runtime handshake or rendering test.

Owners: `crates/vision` for session display/MCP integration, `crates/buildrun` for execution
and process ownership, `crates/agents` for MCP configuration, and `crates/builds` for build
container setup. Keep launcher changes limited to wiring these interfaces.

### Verify upstream contracts

- Pin a compatible Kotlin Toolchain/Compose Hot Reload MCP version and establish how the
  server is obtained and launched, its arguments, transport, readiness, and shutdown.
- Establish whether the server starts the application or attaches to an existing one,
  how it selects a project/module/application, and how it discovers the hot-reload endpoint.
- Inventory its actual MCP tools, filesystem/path expectations, and screenshot/input
  capabilities; do not assume Playwright controls native Compose windows.
- Confirm Linux arm64 support, JetBrains Runtime provisioning, required native libraries,
  software rendering, and source-change detection through the macOS/Podman shared mount.
- Validate required distribution/runtime/MCP artifact destinations through controlled
  networking. Do not substitute the image JDK for the toolchain-provisioned runtime.

### Execution and session ownership

- Run the server and Compose application in the build container, on the requesting
  session's existing display. Agent containers must not gain a JDK or host process access.
- Add an explicit, bounded lifecycle for persistent development applications and MCP
  servers. The current foreground request runner kills descendants on completion and
  holds a queue slot while running: neither detaching an unmanaged process nor indefinitely
  occupying the sole build slot is an acceptable solution.
- Specify admission/resource limits separately for persistent applications and build
  requests. Preserve project CPU/memory limits and coordinate toolchain cache/output
  contention; do not simply release the queue slot while an unbounded build continues.
- Own processes from acquisition through readiness, disconnect, failure, session exit,
  restart, and forced shutdown. Roll back partial starts; retain generation identity so
  stale cleanup cannot terminate another session's replacement process.
- Allocate session-specific endpoints and runtime directories outside source paths. A
  session display/port is routing, not an isolation boundary: project sessions share
  localhost and build state. Document this limitation explicitly.

### MCP transport and agent configuration

- Reuse the existing MCP configuration abstraction after verifying upstream transport.
  For streamable HTTP, bind to pod loopback and register the session URL. For stdio, use
  an explicitly owned stream bridge into the build container; `kotlin-run`'s build-output
  framing and null stdin are not an MCP stdio transport.
- Do not expose Podman sockets or arbitrary remote-shell execution to implement a bridge.
  Keep MCP protocol bytes separate from logs, readiness messages, and build output.
- Publish the server entry only after readiness; report unsupported versions, startup
  failures, or application-selection errors clearly. Preserve unrelated user MCP entries
  and apply existing session-server precedence rules.
- Make this integration opt-in through a trusted setting, following existing policy
  conventions; define the exact key during implementation. Do not launch a persistent
  service merely because untrusted project files request it.

Gate: claim support only after an agent can use the real MCP server against a rendered
Linux JVM Compose application, observe a source edit being reloaded, and cleanly stop the
session without leaking processes or blocking subsequent builds.

## 7. Package and document

- Add the static `kotlin-run` client to `image/Containerfile` and corresponding image
  smoke tests. Add backend/server changes to the build image and stale-image detection
  inputs as required; do not run upstream's floating installer during image construction.
- Update `crates/builds::INSTRUCTIONS` for all agents with both commands, wrapper usage,
  plugin discovery (`kotlin-run show commands`), and custom execution (`kotlin-run do`).
  Preserve unrelated agent configuration changes already in the working tree.
- Update README, configuration, architecture, and repository guidance for toolchain-owned
  JVMs, verified result locations, provisioning destinations, noninteractive commands,
  plugin source edits, and platform/version limitations.

## Verification and acceptance

Unit and process tests in the owning crates:

- Tool selection/protocol compatibility, malformed requests, argument boundaries (spaces,
  empty arguments, Unicode, tabs/newlines), repeated plugin module options, and cwd.
- Nested and mixed project discovery; outside-checkout and symlink escapes; missing wrapper.
- Kotlin child environment excludes image JVM selection and all Gradle-specific injection;
  Gradle argv/environment remain unchanged.
- Shared queue limits, cancellation while queued/provisioning/executing/streaming, exit
  status, descendant cleanup, and bounded release of capacity. Verify detached plugin
  processes cannot silently invalidate the promised cancellation semantics.
- Case-alias and ancestor-rename protections for discovered state and private safety nets.
  Demonstrate each new regression test fails with the corresponding defect reintroduced.

Ignored Podman integration tests using disposable checkouts, scratch AGENT_RUN_HOME, and
isolated namespaces on macOS Apple Silicon/rootless krunkit:

1. Cold bootstrap, JVM build/test/run, warm cache reuse, and checksum failure/retry.
2. Project-local plugin discovery and execution, repeated module selection, generated
   output visibility, intended baseline edits, and failure-status propagation.
3. Host output/cache sentinel preservation including case aliases; mixed Gradle/Kotlin
   execution; queue contention, long-running run/plugin cancellation, restart/reuse.
4. Denied provisioning destination and a successful narrowly approved retry; controlled
   repository behavior; resource limits and cleanup without affecting another project.
5. Follow-on Compose milestone: real MCP handshake/tool invocation, correct application
   and display selection, rendered window and source-change reload, cold JetBrains Runtime
   provisioning, and build/test execution while the development application is active.
6. Compose lifecycle: two sessions with distinct endpoints, partial-start rollback,
   startup timeout, client/application/server disconnect, session exit, project restart,
   forced shutdown, and stale-generation cleanup. Verify bounded process/queue cleanup,
   no host listener exposure, and no impact on another project's applications.

For the Compose milestone, add unit/process tests for endpoint allocation, readiness,
transport framing, preservation of user MCP configuration, admission limits, and lifecycle
failure paths. Keep upstream-dependent tests separate from deterministic bridge tests.

Run `cargo test`, `cargo clippy --all-targets`, `cargo fmt --all`, and
`cargo fmt --all --check`; run image smoke tests and relevant ignored suites. Record
revision, commands/results, pinned Kotlin release, OS/Podman/provider, cold/warm timings,
fixtures and skipped coverage. Bridge tests run below; real Kotlin provisioning and
Podman/macOS acceptance remain unverified.

## Local design review and unresolved gates

- **Plugin semantics corrected:** custom commands are `do <name>`; arbitrary argument
  forwarding, not a hardcoded command catalog, covers project plugins.
- **JVM leakage identified:** existing image JAVA_HOME and PATH must be sanitized for
  Kotlin; fixed image JDKs are not the Kotlin provisioning mechanism.
- **Output setting verified:** `--build-dir` / `KOTLIN_TOOLCHAIN_BUILD_DIR` controls the
  project build root, logs, temp and incremental state in the inspected source. The
  backend sets it; Kotlin manifest roots now also get private build safety nets, preserving
  existing folder spelling and canonically deduplicating explicit folder rules. Their
  workspace/mount-plan regression tests still need macOS execution.
- **Provisioning routing is unresolved:** a distribution-root override does not configure
  all JRE/JDK/dependency downloaders. Validate each client with cold-cache tests.
- **Cancellation needs real plugin tests:** Gradle daemon assumptions cannot be inherited
  without evidence; arbitrary plugin subprocesses are part of the lifecycle test scope.
- **IDE behavior needs verification:** inspect Kotlin Toolchain IntelliJ auto-import/build
  behavior and extend host-tool checks if writable edits can trigger host execution.
  Do not assume the current Gradle reload guard covers Kotlin projects.
- **Alpha API drift:** confirm all contracts against the pinned tested release. A project's
  wrapper remains its version authority; unsupported behavior must fail clearly rather
  than trigger an upgrade or image-JDK fallback.
- **Compose MCP source contract established:** stdio command and pid-file discovery are
  known. Real initialization, protocol/log separation, app attachment, rendering and file
  watching still need testing. Multiple sessions need session-scoped app/MCP build roots
  (or an explicitly shared single app): the upstream pid-file name is fixed per build root.
- **Persistent services require a lifecycle design:** foreground build request handling
  cannot host a lasting MCP/application session unchanged. Resolve queue admission,
  stream ownership, per-session cleanup, and shared-project interference explicitly.
- **Oracle review remains pending:** submit this plan plus the relevant runner, workspace,
  mount-plan, image, and network implementation files when Oracle is available. Resolve
  its findings before treating this proposal as reviewed by Oracle.

## Milestone 1 execution record

Investigation environment: Linux aarch64 agent container. Existing unrelated changes in
`crates/agents/src/lib.rs` are preserved.

- Successfully fetched the toolchain documentation search index. Confirmed cache controls,
  project-local wrapper version/checksum authority, and custom plugin commands via `do`.
- Documentation exposes `--shared-cache-dir`, but the inspected CLI/provisioning pages and
  search index do not establish a supported build-output redirection option. Output
  isolation remains a gate, not an assumed capability.
- Compose documentation confirms `run --compose-hot-reload`, automatic JetBrains Runtime
  provisioning, and source watching. The search index contains no documented contract for
  `compose-hot-reload-mcp-server`; transport/launch support is still unverified.
- Reading the current runner confirms foreground requests hold a queue slot and terminate
  their process group when finished. Its null stdin and framed output cannot directly
  serve as an MCP stdio bridge.
- Fetching `https://kotl.in/install.sh` and the Kotlin CLI Maven metadata from
  `packages.jetbrains.team` each returned HTTP 403. Nothing was installed or executed.
- Reading `JetBrains/kotlin-toolchain` through the GitHub relay returned HTTP 403 because
  the repository is not in its approved map. Source inspection is blocked.
- `cargo`, `rustc`, `rustfmt`, and `podman` are unavailable in this container. The checkout
  has no Gradle wrapper, so its Gradle-only build bridge is not a Rust validation path.
  No Rust tests, formatting, image builds, or disposable runtime fixtures were executed.

To resume, approve the upstream repository in the host's existing
`[network.github].repos` list (append `JetBrains/kotlin-toolchain`, preserving other
entries) and make the required provisioning destinations available through policy.
Provide a Rust-capable validation environment for implementation; real integration
acceptance still requires the documented macOS/rootless Podman release gate. Do not
weaken output protection or substitute the image JVM to work around these blockers.

### Follow-up after research permissions were added

- The Kotlin documentation site/search index is now accessible to the agent.
- Confirmed project JDK selection modes: `auto`, `alwaysProvision`, and `javaHome`.
  Provisioning uses the Foojay Discovery API; distributions and major versions are
  project-controlled. Removing image JAVA_HOME must not silently substitute another image
  JDK, and projects explicitly requiring `javaHome` need a clear unsupported/missing-JDK
  error rather than an automatic policy/configuration change.
- GitHub relay requests for `JetBrains/kotlin-toolchain` still return HTTP 403. A real
  build-side curl probe to the approved distribution metadata URL also returned HTTP 403.
  The temporary dependency-free probe source was removed; no generated project files
  were retained and existing staged source changes were preserved.
- Manual GitHub/build allowlist edits need reconciliation by starting a session. Run
  `agent pi true` on the host in this checkout to refresh the relay and build rules without
  ending the current session. No additional broad/shared grants are requested.
- Output redirection and the Compose MCP launch/transport contract remain unverified;
  Kotlin runtime implementation has not begun.

### Read-only upstream snapshot and preparatory implementation

The host-provided checkout at `/knowledge/github/kotlin-toolchain` is available even though
the relay remains denied. Its HEAD is `7133c358a9605541d37d144d73f5860df5be6965`, dated
2026-11-10, with a wrapper pinned to `0.14.0-dev-4624` and SHA-256
`e7a8cd08d45959f2f380e513a44a39856796f62aa1ab1b1def0f739e0307eaef`.
This differs from the earlier 0.13 documentation; do not assume identical contracts in
older releases or automatically upgrade a project's wrapper to this development version.
No further relay approval is needed to inspect this provided snapshot.

Source contracts inspected:

- `kotlin`: project-local version/checksum detection; distribution download with checksum
  verification, extraction completion marker and lock; exec of the distribution launcher.
- `sources/amper-wrapper/resources/wrappers/launcher.template.sh`: provisions its own
  checksum-pinned Zulu JRE unless KOTLIN_CLI_JAVA_HOME overrides it. This snapshot uses
  `cdn.azul.com` for that runtime, separately from project JDK provisioning.
- `sources/amper-cli/src/org/jetbrains/amper/cli/options/ProjectLayoutOptions.kt`:
  `--build-dir` and `KOTLIN_TOOLCHAIN_BUILD_DIR`; `--project-dir` is independent of wrapper
  version discovery. `ProjectCliContext.kt` puts logs/temp/incremental state below that root.
- `ComposeHotReloadMcpServerCommand.kt`: actual stdio command, minimum hot-reload version,
  provisioned JDK, downloaded MCP artifact and pid-file discovery. Its upstream test and
  wizard MCP configuration confirm the intended stdio setup. Cold-start bootstrap/log
  output still needs protocol-level validation; do not heuristically strip protocol bytes.
- `sources/core/src/org/jetbrains/amper/core/downloader/ktor.kt`: Ktor/OkHttp downloader
  configuration does not explicitly set proxy authentication.
- `sources/dependency-resolution/src/org/jetbrains/amper/dependency/resolution/files.kt`:
  the JDK HttpClient builder has proxy/authenticator configuration commented out. Proxy
  environment variables or a JVM-wide default authenticator are not evidence that these
  clients will authenticate. A supported authenticated route must be demonstrated before
  enabling cold builds; do not open a pod-local anonymous credential-forwarding listener,
  since agents share that network namespace. Prefer supported upstream proxy hooks over
  reflection into private client state or bytecode patching.

Implemented backend foundation (subsequently wired into the bridge below):

- `crates/buildrun/src/kotlin.rs`: bounded canonical project/wrapper discovery, no global
  fallback, rejection of missing/nonexecutable or out-of-checkout symlink wrappers.
- Argument-preserving command construction, project-scoped build output and checkout-owned
  bootstrap/shared caches, telemetry/banner opt-out, and removal of image JVM selectors
  from environment/PATH. No general agent environment forwarding or image-JDK fallback.
- Existing canonical cwd validation is reused, not copied. Initial API-only work has now
  been followed by protocol/client packaging and launcher-instruction updates below.
- Five tests include module/nested discovery, missing/escaping wrappers, no-marker
  wrappers, exact plugin argv/status/cwd through a stand-in executable, and output/JVM
  environment preparation. These do not execute a real Kotlin distribution or prove
  cold provisioning, plugin compilation, output mounts or MCP support.

Verification via the existing build container:

- `cargo-run test --locked -p buildrun -p egress -p policy`: **44 passed**, one Podman
  integration test ignored (16 buildrun unit, 5 process, 10 egress unit, 13 policy unit).
- `cargo-run clippy --locked -p buildrun -p egress -p policy --all-targets -- -D warnings`
  and `cargo-run fmt --all --check`: **passed**.
- Regression mutations that retained image JAVA_HOME, used cwd-scoped output, and accepted
  an escaping wrapper each compiled and failed their targeted test as expected. An initial
  cwd mutation did not compile and was not counted; the corrected mutation did compile
  and fail. All mutations were restored; all five Kotlin tests pass again afterward.
- `git diff --check`: passed. Temporary research project directories were removed.

### Project-session bridge and instructions

The user's attempted project build exposed that stopping at a preparatory API left the
actual task unfinished. The working tree now includes:

- A `kotlin-run` binary and versioned Kotlin protocol selector in `buildrun`; server
  execution uses the existing streaming/status/display/cancellation path and shared queue.
  Plugin arguments are added exactly once. Kotlin uses the project wrapper, not the
  Gradle test-command override, and stdin remains closed for foreground builds.
- Agent image build/COPY steps and image smoke presence checks include the new client.
  Both agent and build image input hashes already track all buildrun source files.
- `crates/builds::INSTRUCTIONS` now tells every agent to use `kotlin-run build/test`,
  `show commands`, and `do <plugin-command>`, without installing/substituting a JVM.
  Instructions identify result/cache paths, build-only network configuration, noninteractive
  execution and the unsupported Compose MCP transport.
- Workspace roots include Gradle settings and Kotlin `project.yaml`/`module.yaml`, with
  private build safety nets even for absent folders. Explicit folder rules are compared
  canonically to prevent duplicate auto-protection mounts. Added workspace and mount-plan
  tests cover Kotlin-only projects, nested modules, case spelling and explicit rules.
- README/configuration/architecture/repository guidance describe the enabled command,
  initial-integration status, project-controlled JVMs, download policy and known gaps.

Verification in the current build container:

- `cargo-run test --locked -p buildrun -p policy -p egress`: **47 passed**, one Podman
  test ignored (17 buildrun unit, 7 process, 10 egress unit, 13 policy unit).
- `cargo-run clippy --locked -p buildrun -p policy -p egress --all-targets -- -D warnings`,
  `cargo-run fmt --all --check`, `git diff --check`, shell smoke-script syntax: **passed**.
- Real client/server tests execute a project-wrapper stand-in with exact plugin argv,
  nested cwd, isolated state, absent JVM overrides, display, EOF stdin and returned exit
  status. Another test cancels Kotlin while Cargo waits, proving shared queue release.
- Disabled protocol dispatch, duplicate argv forwarding, and early queue-slot release were
  each introduced temporarily; each mutant compiled and failed its targeted process test.
  Sources were restored; the full 47-test suite and Clippy then passed again.
- macOS workspace/mount-plan and instruction tests, image builds/smoke, actual cold Kotlin
  distributions/plugins and Compose MCP are **not** covered by these passing stand-ins.
  In particular the JVM proxy compatibility issue above is not fixed or validated here.

To activate in another checkout: update the launcher/images, run `agent restart` there,
and start a new session. It will receive `kotlin-run` and the matching instructions/server.
Ask it to attempt `kotlin-run build` and retain the real output/status. Do not treat a
missing client as a project error, or a successful stand-in as a successful Kotlin build.

## Sources consulted

- https://kotlin-toolchain.org/latest/
- https://kotlin-toolchain.org/latest/cli/
- https://kotlin-toolchain.org/latest/cli/provisioning/
- https://kotlin-toolchain.org/latest/user-guide/advanced/jdk-provisioning/
- https://kotlin-toolchain.org/latest/user-guide/plugins/overview/
- https://kotlin-toolchain.org/latest/user-guide/plugins/topics/custom-commands/
- https://kotlin-toolchain.org/latest/user-guide/builtin-tech/compose-multiplatform/
  (hot-reload sections inspected through the documentation search index)

The consulted pages identify release 0.13. Capture an immutable release reference during
milestone 1 rather than relying on the moving `latest` URL for implementation tests.
