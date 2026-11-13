# agent-run

Run Claude Code, Codex, Gemini CLI, or Pi in rootless Podman containers on macOS.
Agents edit your live checkout, while container mounts and network policy restrict their
access. Gradle builds run separately from your host's build state.

Agents can still change source and build scripts. Review their changes before running
host tools; containers do not make those changes safe to execute on your Mac.

## Requirements

- macOS on Apple Silicon.
- [Podman](https://podman.io/) and [krunkit](https://github.com/containers/krunkit),
  with a running rootless machine using the `libkrun` provider.
- Rust 1.89 or newer, including Cargo.
- A Git checkout to work on. Run agent-run from its root directory.

## Quick start

### 1. Prepare Podman

If you use Homebrew:

```sh
brew install podman slp/krunkit/krunkit
```

Select the krunkit-backed provider in this terminal and in your shell startup file
(such as `~/.zshrc`):

```sh
export CONTAINERS_MACHINE_PROVIDER=libkrun
```

For a new installation, create and start a machine:

```sh
podman machine init --provider libkrun
podman machine start
```

If a machine already exists, do not initialize another one. Check it with
`podman machine list`, and start it if stopped. Its `VM TYPE` must be `libkrun`;
a machine created with another provider does not become a libkrun machine by changing
the environment variable. Confirm rootless mode:

```sh
podman info --format '{{.Host.Security.Rootless}}'
```

The result must be `true`. Builds share the machine's CPU and memory allocation; large
projects may need more memory than its initial allocation.

### 2. Add agent-run to your PATH

From this repository's root, run:

```sh
export PATH="$PWD/bin:$PATH"
```

To keep it across terminals, add an equivalent line with the repository's **absolute
path** to your shell startup file, such as `~/.zshrc`.

### 3. Create your policy

```sh
mkdir -p ~/.agent-run
```

Save this as `~/.agent-run/policy.toml` for a Claude session:

```toml
version = 1

[agents]
memory = "2g"
cpus = 2

[agents.claude]
allow = ["api.anthropic.com", "claude.ai", "console.anthropic.com", "platform.claude.com"]

[workspace]
mask = [".signing", ".idea", ".venv", "target"]
read-only = [".git", "gradle/wrapper"]
```

For other agents, add their hosts using the [configuration guide](docs/configuration.md).
That guide also covers Gradle dependencies, resources, local services, and engram.

Optional global instructions go in `~/.agent-run/instructions.md`; agents receive them
read-only.

### 4. Start a session

Change to the root of the project you want the agent to edit, then run:

```sh
agent claude
```

The first launch compiles the launcher and builds container images. Downloads and image
builds can take several minutes. Later launches reuse images unless their inputs change.

For Claude, enter `/login` inside the running Claude session and follow its instructions.
Do not start a Bash shell to log in. For other agents, use their own supported login or
credential settings inside the session.

Browser callbacks to the container are not supported. Login and settings persist under
`~/.agent-run/<agent>/home`; your normal host-agent home is not automatically imported.
Provider API keys exported in your Mac's shell are **not forwarded** into sessions.

## Daily use

Run one of these from your project's root:

```sh
agent claude
agent codex
agent gemini
agent pi
```

Use a shell or another command instead of the agent CLI:

```sh
agent claude bash
agent claude bash -lc 'pwd; git status --short'
```

agent-run uses the current checkout and branch. It does not create worktrees or switch
branches. Edits are visible immediately in both your IDE and the container.

### Approve network access

Run approval commands **on your Mac**, in the same project:

```sh
agent denied
agent allow example.org
agent allow example.org --project
agent revoke example.org
agent policy show
```

- `allow` without a scope is temporary: it lasts until the project pod is removed.
- `--project` stores the approval in policy for that agent and project.
- If several agent types are running, select one explicitly:

  ```sh
  agent allow example.org --agent pi
  agent revoke example.org --agent pi
  ```

- `agent revoke example.org --global` removes a shared-list approval.

Successful approval/revocation applies without restarting sessions. Already-open
connections can remain usable after revocation until they close. If applying a change
fails, treat the error as unresolved rather than assuming access changed.

### Restart or shut down

```sh
agent restart
```

This **ends all agent sessions in the current project** and removes its pod. The next
launch creates it again. Use it after workspace-rule changes, to protect newly created
rule folders, or when told that a running build container uses an older image.

```sh
agent shutdown
```

Stops agent-run infrastructure when no sessions are active. To explicitly end active
sessions too:

```sh
agent shutdown --force
```

Persistent homes, policy, cache volumes and agent logins are retained.

## Gradle builds

Agents invoke this **inside their container**:

```sh
gradle-run test
```

The build uses the project's shared build container, not the host Gradle installation.
The image provides JDKs 11, 17, 21, and 25. Dependencies and Gradle distributions come
through the configured mirror; see [Gradle configuration](docs/configuration.md#gradle-dependencies).
Project-declared mise tools are not installed.

- Build state is stored under `/build-state`, separately from host build folders/caches.
- All agent types and sessions on the same checkout share container build state and caches.
  Different checkouts have separate state.
- Agents can read build results; builds write them.
- By default, one build runs per project at a time; additional requests wait.
- Output streams back to the caller. Ctrl+C requests cancellation. Interactive build
  prompts and general forwarding of caller environment variables are not supported.

Android builds use a container SDK. Accept required SDK licenses on your Mac first;
agent-run copies licenses from the host SDK and refreshes them on later launches. It does
not expose your host SDK's writable contents to builds.

## Browser and display

Each session gets a display and a browser through its `playwright` MCP server. The browser
can reach services running in the project's containers at `localhost`. The session can
also capture windows opened by its builds using `scrot`. Screenshots are available under
`/build-state/screenshots/<session>`.

Sessions in the same project share a pod network and can access each other's displays.

## Optional engram integration

engram must already be installed and configured for the project **on the host**.
agent-run does not activate it or replace your project's `.claude` setup.

Build engram's server with `./gradlew installDist` in `engram-core/mcp-server`, then add the
project's package path to [policy](docs/configuration.md#engram).

Agents receive instructions and skills read-only and update context through engram's MCP
tools. A save request triggers a commit on your Mac using engram's save workflow and your
Git signing setup. Configure signing and any pinentry UI on the host beforehand; prompts
or failures can delay saves. Saves run during the session, but the launcher prints their
success/failure summaries when the session ends. Silence during a session does not confirm
a successful save. These are engram-context commits, not commits to the checkout the
agent is editing.

## Security and limitations

- Agents work on your live files. They cannot commit or push the workspace through the
  intended protected-Git setup; engram context saves are a separate host operation.
- Masked folders are hidden; read-only folders cannot be edited; private folders use
  container-owned storage instead of host contents.
- Host Gradle state and existing supported host-agent configuration folders are protected.
  Folder protections are planned at pod creation; newly appearing folders can require
  `agent restart`.
- Network access is limited to shared policy destinations, the agent's own destinations,
  and its approvals. Allowed services can still receive data an agent can read.
- GitHub relay access is read-only for the repositories listed in policy. It does not
  forward your GitHub credentials.
- On case-insensitive macOS filesystems, single-file mounts are not reliable protection
  against alternate spellings. `local.properties` is therefore accepted only with
  `sdk.dir` and `ndk.dir` entries; do not store secrets there.
- Disable IntelliJ's automatic Gradle reload before starting a session. An automatic host
  sync could execute an agent-edited build script.
- Keep the agent-run installation, policy/state, and host-executed tooling outside the
  writable workspace.
- Review `git diff` and `git status --ignored` before running host builds, scripts, or
  tools on agent-edited files. Agents and builds can execute arbitrary project code
  inside their containers.

## Troubleshooting

| Problem | What to check |
|---|---|
| `agent` is not found | Check that the repository's absolute `bin` path is on `PATH` in this terminal. |
| Cargo cannot build the launcher | Check `rustc --version` and `cargo --version`; Rust 1.89+ is required. |
| Podman is unavailable or rootful | Check `podman machine list`, start your machine, and verify rootless mode as above. |
| A request or login is denied | Run `agent denied` and `agent policy show` on the host in the project. Add only the destination you intend to permit. |
| IntelliJ reload check fails | Turn off Settings > Build Tools > "Reload project after changes in the build scripts". Masking `.idea` does not disable host reloads. |
| Gradle dependencies are unavailable | Add the project's repositories to `mirror.remotes`, then run `agent restart` before relaunching; this ends its active sessions. Arbitrary repository URLs are not automatically allowed. |
| A required JDK/tool is unavailable | Check the image's tools. Extend the image rather than expecting project mise configuration to install them. |
| Android SDK packages cannot install | Check the host SDK path and accepted licenses, then start another session to refresh copied licenses. |
| engram fails to start/save | Check host installation, package path, built MCP server, immutable links, and host signing setup. |
| Workspace rules changed or an image is stale | Finish work, then run `agent restart`; this ends the project's sessions. |

The launcher prints startup steps and errors to the terminal; engram save summaries appear
when the session ends. Include the failing step and message when reporting a problem;
do not share credentials or entire agent homes.

## Stored files

| Path | Purpose |
|---|---|
| `~/.agent-run/policy.toml` | Access rules and resource limits |
| `~/.agent-run/instructions.md` | Optional global agent instructions |
| `~/.agent-run/knowledge` | Shared reference material, read-only at `/knowledge` |
| `~/.agent-run/<agent>/home` | Persistent login and settings; treat as sensitive |
| `~/.agent-run/state` | Internal plans, approvals, and service/session configuration |

For Pi, edit user MCP servers on the host in
`~/.agent-run/pi/home/.pi/agent/mcp.json`. The host file stays unchanged. Other user MCP
entries remain available, but session-provided `playwright` and, when enabled, `engram`
entries take precedence over the same names in the session configuration. Do not edit
internal `state` files while agent-run is running.

## Contributing

See [Architecture and security model](docs/architecture.md) for component responsibilities,
trust boundaries, and verification requirements. [AGENTS.md](AGENTS.md) documents repository
conventions and test commands.
