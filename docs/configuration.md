# Configuration

agent-run reads `~/.agent-run/policy.toml` on your Mac. Keep it outside the writable
workspace. Unknown keys and invalid values are errors. `agent policy show` displays the
effective grants for the current project.

Start with the [README quick start](../README.md#quick-start). The sections below extend
that policy. Avoid defining the same TOML table twice.

## Agent network access

Shared destinations are available to every agent and to builds:

```toml
[network.project]
allow = ["registry.npmjs.org"]
```

### Cloud providers

The README uses a local model. To use a cloud provider, add its hosts to that agent's
`allow` list and use its login or credential settings inside the session. In Pi, use
`/login` to sign in and `/model` to choose a model.

Use shared grants sparingly. Agent-specific destinations belong in their own sections:

```toml
[agents.claude]
allow = ["api.anthropic.com", "claude.ai", "console.anthropic.com", "platform.claude.com"]

[agents.codex]
allow = ["api.openai.com", "auth.openai.com", "chatgpt.com"]

[agents.gemini]
allow = ["generativelanguage.googleapis.com", "accounts.google.com", "oauth2.googleapis.com", "cloudcode-pa.googleapis.com"]

[agents.pi]
allow = ["auth.openai.com", "chatgpt.com"]
```

Pi's required hosts depend on your chosen providers and login method.
Use `agent denied` to diagnose missing destinations.
Each agent gets the shared list plus its own grants, not another agent's list.

Entries are hostnames with optional ports, for example `example.org:8443`. Ordinary
access uses HTTPS. They are not URL paths. Access to an allowed host can include sending
data to that host.

### Pi with Codex

To use Codex models through Pi, keep the local endpoint and add the OpenAI login hosts
to the same Pi table:

```toml
[agents.pi]
local = ["host.containers.internal:11434"]
allow = ["auth.openai.com", "chatgpt.com"]
```

Start `agent pi`, use `/login` to sign in with your OpenAI/ChatGPT account, then use
`/model` to select a Codex model available to your account. Switch back to the local model
through `/model` whenever you want. Requests to Codex leave your Mac.

This runs Pi with an OpenAI model. To run the separate Codex CLI, use `agent codex` and
configure `[agents.codex]` instead.

### Local services

Explicitly allow local HTTP services per agent:

```toml
[agents.pi]
local = ["host.containers.internal:11434"]
```

This permits that agent to contact an LLM service on your Mac at the named port. Local
exceptions deliberately expose a host service to untrusted code. Enable only what you
need. Replace the Pi table above with this block.

## Other agents

Add the agent's network hosts from [Agent network access](#agent-network-access), then run
its command from the project root:

```sh
agent claude
agent codex
agent gemini
```

For Claude Code, enter `/login` inside the running Claude session and follow its
instructions. Use the agent's own login flow instead of starting a Bash shell to sign in.
For Codex and Gemini CLI, use their supported login or credential settings inside the
session. Browser callbacks to the container are unavailable.

Each agent keeps its own login and settings in `~/.agent-run/<agent>/home`. Host-exported
provider API keys are not forwarded into containers.

## Resources

Defaults can be written explicitly:

```toml
[agents]
memory = "2g"
cpus = 2

[builds]
memory = "16g"
cpus = 8
max-concurrent = 1
```

Agent limits apply per session. Build limits apply to each project's build container.
Its browser/display processes also consume that allocation. `max-concurrent` is the
number of builds allowed simultaneously per project, not across all projects.

These are caps, not reservations. All containers still share the Podman machine's CPU
and memory limits. Multiple projects can exhaust the VM even when each respects its
container limit. Memory settings come from policy, not `AGENT_MEMORY`.

Set a build-memory override for one checkout:

```toml
[projects."~/Projects/app"]
builds.memory = "20g"
```

Resource changes affect newly created containers. Restart the project to replace an
existing build container with updated limits.

## Workspace folders

```toml
[workspace]
mask = [".signing", ".idea", ".venv", "target"]
read-only = [".git", "gradle/wrapper"]
private = ["kotlin-js-store"]
```

Paths are exact folders relative to the checkout root, not glob patterns:

| Rule | Container view |
|---|---|
| `mask` | Host contents are hidden: the folder is empty and read-only. |
| `read-only` | Host contents are visible but cannot be changed. |
| `private` | Container-owned storage replaces the host folder. Host contents are not copied into it. |

Private state is associated with the project and can survive sessions. Agents in the project share it. Everything else in the live checkout can be
edited unless automatically protected. Do not weaken Git metadata protection with
conflicting private/mask rules or case aliases.

Existing host Gradle state, Rust `target` folders, and supported real host-agent configuration folders receive
automatic protection. Paths used as protected mount targets must not traverse symlinks.
Engram-managed immutable links use their separate integration checks.

Workspace-rule changes require `agent restart`, which ends active sessions in the
project. A newly created matching folder may remain unprotected until restart even
when the rule itself has not changed.

## Build network access

Build-only destinations belong in the existing `[builds]` table, not the shared
`[network.project]` list:

```toml
[builds]
allow = ["index.crates.io", "static.crates.io"]
```

For additions used by just one checkout:

```toml
[projects."~/Projects/app".builds]
allow = ["packages.example.org"]
```

The two build lists merge; neither is added to agents' direct proxy grants. Shared
`network.project.allow` grants still apply to both agents and builds. If these hosts were
previously added there, remove those entries to make their access build-only. Temporary
`agent allow` approvals continue to apply to agent sessions, not builds.

The build container uses its own proxy credentials, stored only in trusted host state and
passed to that container. Dropping authentication does not grant build-only hosts, and
an agent's credentials cannot use them. Clients must honor the authenticated HTTP proxy;
Cargo/curl support this through their proxy environment variables. Java-specific clients
may need their own authentication configuration; Gradle dependency mirroring is unchanged.

This is not isolation from an agent's build code: agents can run arbitrary builds and
reach build-approved destinations indirectly, and project sessions share build services.
Use exact HTTPS hosts/ports; build grants do not authorize private/local services.

After updating the launcher, run `agent restart` once to replace older build containers
that lack the build proxy identity. Later allowlist edits reconcile when a session starts;
already-open tunnels can survive revocation until they close. `agent policy show` displays
build-only grants separately and `agent denied` points build denials to `builds.allow`.

## Gradle dependencies

The dependency mirror fetches from explicitly configured remotes:

```toml
[mirror.remotes]
central = "https://repo.maven.apache.org/maven2"
google = "https://dl.google.com/dl/android/maven2"
gradle-plugins = "https://plugins.gradle.org/m2"
gradle-distributions = "https://services.gradle.org/distributions"
android-sdk = "https://dl.google.com/android/repository"
```

Keep the remotes your project uses and add others deliberately. Gradle repository
routing uses these IDs/URLs. An unknown repository does not automatically gain access.
The Gradle-distribution remote supports wrapper downloads, and the Android remote
supports SDK package downloads.

Configure project dependencies here rather than granting their Maven hosts directly to
every agent. Host builds continue using their own configuration and caches.

## Rust dependencies

Cargo does not use the Maven/Gradle dependency mirror. To fetch public crates, merge these
hosts into your existing build-only list (see [Build network access](#build-network-access)):

```toml
[builds]
allow = ["index.crates.io", "static.crates.io"]
```

Other registries and Git dependencies need their own deliberate grants. The read-only
GitHub API relay is not a Git transport and cannot satisfy Cargo Git dependencies.
Host Cargo credentials/configuration are not imported. Project `.cargo/config.toml` is
untrusted build configuration and can still define aliases, registries, and build tools.

The build image installs a pinned Rust toolchain with rustfmt and Clippy using versioned
rustup bootstrap artifacts checked against their published SHA-256, followed by rustup's
verified toolchain installation. Downloads happen during the trusted image build, not
through a session's dependency mirror. Runtime toolchains are read-only under
`/opt/rustup`; automatic installation is disabled. To support another project-selected
version or component, explicitly install it when building the image, then restart the
project. `cargo-run` does not ignore project toolchain declarations or silently choose a
different version.

Cargo's writable cache and default target directories are isolated under `/build-state/rust`.
CLI/configuration overrides and arbitrary build scripts are not constrained to these
output paths; existing standard host `target` folders are hidden by the mount plan.
Nonstandard host output folders need explicit workspace protection. New folders may need
`agent restart` before receiving mount protection.

## Kotlin Toolchain dependencies

Kotlin Toolchain can't use the egress proxy, so `kotlin-run` points each of its downloads at
the dependency mirror (Kotlin Toolchain 0.14.0 or newer lets every download address be
replaced). The mirror needs a remote for each upstream the project uses:

```toml
[mirror.remotes]
central = "https://repo.maven.apache.org/maven2"
google = "https://dl.google.com/dl/android/maven2"
kotlin-toolchain = "https://artifacts-caching-proxy.aws.intellij.net/packages.jetbrains.team/maven/p/amper/amper"
jetbrains-download = "https://download.jetbrains.com"   # the JDK list
google-downloads = "https://dl.google.com"              # the Android SDK
nodejs = "https://nodejs.org/dist"                      # web targets
pnpm = "https://github.com/pnpm/pnpm/releases/download" # web targets
gradle-distributions = "https://services.gradle.org/distributions"  # Android builds

[builds]
allow = ["registry.npmjs.org"]  # pnpm's packages, through the proxy
```

Remote names are free: `kotlin-run` finds each by its upstream address and warns about any
it can't find. The Kotlin Toolchain repository comes through JetBrains' caching proxy: the
repository itself redirects large files to a CDN host that changes, which the mirror's
allowlist can't follow. The wrapper checks the distribution against its pinned checksum.

The mirror serves HTTPS with agent-run's own certificate authority (in agent-run's state;
build containers trust it): Kotlin Toolchain resolves from HTTPS repositories only. Builds
have a writable home in the project's build state, so tools that write under `~` (Android's
`~/.android`, Maven's `~/.m2`, pnpm's `~/.local`) work; agents don't see it.

Kotlin Toolchain uses `JAVA_HOME` when it matches a project's JDK criteria: `kotlin-run`
sets it to the image's Temurin 25, which also runs the Kotlin CLI. A project requiring
another JDK version would download one from its vendor's host, which no mirror serves.

`kotlin-run` forwards all CLI arguments, including project plugin commands through `do`.
The default output/cache locations and foreground-command limitations are described in
[the README](../README.md#kotlin-toolchain-builds). Compose MCP is not a stdio service
through this framed, stdin-closed build protocol.

## GitHub access

```toml
[network.github]
repos = ["owner/repository"]
```

The relay permits read-only GitHub API access to listed repositories and does not
forward host credentials. This is not authenticated private-repository access or
permission to push. Agents receive its address through `GITHUB_API_URL`.

List repositories every project uses here, and a project's own in its table; those are
served only to that project's pod (the relay tells pods apart by their network):

```toml
[projects."~/Projects/app".network.github]
repos = ["owner/tool-this-project-reads"]
```

Edits apply when a session next starts (or with any `agent allow`/`agent revoke`), also for
sessions already running.

## Project-specific grants

Project tables use checkout paths. `~/` expands to your home directory:

```toml
[projects."~/Projects/app"]
network.project.allow = ["downloads.example.org"]
network.github.repos = ["owner/repository"]
builds.memory = "20g"

[projects."~/Projects/app".agents.pi]
allow = ["provider.example.org"]
```

Shared project grants are available to all agents/builds in that checkout. Agent entries
add access only for that agent in the project. You can also create agent/project grants
with `agent allow HOST --project --agent NAME` from the checkout.

## Engram

First install/configure engram for the project on the host and build its MCP server.
agent-run neither activates engram nor replaces your host configuration.

Declare the existing package path:

```toml
[projects."~/Projects/app"]
engram = "~/engram/projects/app"
```

If you already have that project table, add the `engram` key to it instead of repeating
the header. The path is the project's engram package, not the checkout or `engram-core`.

The expected instructions/tool links must already exist and be locked by host engram
setup. Agents receive instructions and skills read-only and make context changes through
the engram tools. Save requests run engram's host-side save workflow with your signing
configuration. Ensure its signing/pinentry setup works on the Mac before using it.

## Applying changes

| Change | Action |
|---|---|
| Temporary or persistent destination approval | Use `agent allow`/`agent revoke`. Successful application needs no session restart. |
| Manually edited network/relay settings | Reconcile by starting a session. Use launcher errors to detect failed application. Do not assume file edits are applied immediately. |
| Mirror remote mappings | Run `agent restart` in each affected project, then start a new session. A running build container retains the mappings from its creation. Another session alone does not update them. |
| Workspace protection rules or newly appeared protected folders | `agent restart` before starting the next session. |
| Build memory/CPU or a stale running build image | `agent restart` to recreate project containers. |
| Agent resource limits | Start a new session. Existing containers retain their limits. |

`agent restart` ends all sessions in the current project. `agent shutdown` stops idle
infrastructure. `agent shutdown --force` explicitly ends active sessions too. Persistent
homes and cache volumes remain.
