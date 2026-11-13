# Configuration

agent-run reads `~/.agent-run/policy.toml` on your Mac. Keep it outside the writable
workspace. Unknown keys and invalid values are errors. `agent policy show` displays the
effective grants for the current project.

Start with the [README quick start](../README.md#quick-start). The sections below extend
that policy; avoid defining the same TOML table twice.

## Agent network access

Shared destinations are available to every agent and to builds:

```toml
[network.project]
allow = ["registry.npmjs.org"]
```

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

Entries are hostnames with optional ports, for example `example.org:8443`; ordinary
access uses HTTPS. They are not URL paths. Access to an allowed host can include sending
data to that host.

### Local services

Explicitly allow local HTTP services per agent:

```toml
[agents.pi]
allow = ["auth.openai.com", "chatgpt.com"]
local = ["host.containers.internal:11434"]
```

This permits that agent to contact an LLM service on your Mac at the named port. Local
exceptions deliberately expose a host service to untrusted code; enable only what you
need. Replace the Pi table above with this block.

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

Agent limits apply per session. Build limits apply to each project's build container;
its browser/display processes also consume that allocation. `max-concurrent` is the
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
| `mask` | Host contents are hidden behind an empty protected mount. |
| `read-only` | Host contents are visible but cannot be changed. |
| `private` | Container-owned storage replaces the host folder. Host contents are not copied into it. |

Private state is associated with the project and can survive sessions; it is not a
fresh scratch directory for every agent. Everything else in the live checkout can be
edited unless automatically protected. Do not weaken Git metadata protection with
conflicting private/mask rules or case aliases.

Existing host Gradle state and supported real host-agent configuration folders receive
automatic protection. Paths used as protected mount targets must not traverse symlinks.
Engram-managed immutable links use their separate integration checks.

Workspace-rule changes require `agent restart`, which ends active sessions in the
project. A newly created matching folder may remain unprotected until restart even
when the rule itself has not changed.

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
routing uses these IDs/URLs; an unknown repository does not automatically gain access.
The Gradle-distribution remote supports wrapper downloads, and the Android remote
supports SDK package downloads.

Configure project dependencies here rather than granting their Maven hosts directly to
every agent. Host builds continue using their own configuration and caches.

## GitHub access

```toml
[network.github]
repos = ["owner/repository"]
```

The relay permits read-only GitHub API access to listed repositories and does not
forward host credentials. This is not authenticated private-repository access or
permission to push. Agents receive its address through `GITHUB_API_URL`.

## Project-specific grants

Project tables use checkout paths; `~/` expands to your home directory:

```toml
[projects."~/Projects/app"]
network.project.allow = ["downloads.example.org"]
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
| Temporary or persistent destination approval | Use `agent allow`/`agent revoke`; successful application needs no session restart. |
| Manually edited network/relay settings | Reconcile by starting a session; use launcher errors to detect failed application. Do not assume file edits are applied immediately. |
| Mirror remote mappings | Run `agent restart` in each affected project, then start a new session. A running build container retains the mappings from its creation; another session alone does not update them. |
| Workspace protection rules or newly appeared protected folders | `agent restart` before starting the next session. |
| Build memory/CPU or a stale running build image | `agent restart` to recreate project containers. |
| Agent resource limits | Start a new session; existing containers retain their limits. |

`agent restart` ends all sessions in the current project. `agent shutdown` stops idle
infrastructure; `agent shutdown --force` explicitly ends active sessions too. Persistent
homes and cache volumes remain.
