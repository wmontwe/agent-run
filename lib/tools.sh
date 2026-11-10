# Build toolchains. A project's tools (its mise.toml, pinned with checksums in
# mise.lock) are installed by `mise install --locked` in a throwaway container
# before the agent starts, into the per-agent volume agent-run-<agent>-mise,
# which the agent gets read-only at /mise. That container sees only copies of
# the project's mise files, never the workspace, and downloads through
# proxy-tools; agents never reach those hosts (GitHub among them). The same
# container copies the user's accepted Android SDK licenses into
# agent-run-<agent>-android (/android; the Android Gradle Plugin downloads SDK
# packages itself) and lets the project's Gradle wrapper download its
# distribution into the agent's Gradle home for the workspace (~/.gradle,
# lib/gradle.sh). Java, Kotlin and
# Gradle settings are the project's; nothing here configures them.

# The proxy for every JVM, Gradle's daemons and test workers included: Java
# ignores HTTPS_PROXY. compose.yaml sets the same for agents.
JAVA_PROXY_OPTIONS="-Dhttp.proxyHost=proxy -Dhttp.proxyPort=3128 -Dhttps.proxyHost=proxy -Dhttps.proxyPort=3128 -Dhttp.nonProxyHosts=localhost|127.0.0.1"

# copy_tool_config WS DEST: copies the project's mise files and Gradle wrapper
# (properties and jar), those that are plain files, into DEST. Sets TOOL_CONFIG to
# "locked" (a mise config and mise.lock), "unlocked" (a config without lock)
# or "none".
copy_tool_config() {
    local ws="$1" dest="$2" file config=false
    TOOL_CONFIG=none
    mkdir -p -- "$dest"
    if [[ ! -L "$ws/gradle" && ! -L "$ws/gradle/wrapper" ]]; then
        for file in gradle-wrapper.properties gradle-wrapper.jar; do
            [[ -f "$ws/gradle/wrapper/$file" && ! -L "$ws/gradle/wrapper/$file" ]] || continue
            cp -- "$ws/gradle/wrapper/$file" "$dest/$file"
        done
    fi
    for file in mise.toml .mise.toml mise.lock; do
        [[ -f "$ws/$file" && ! -L "$ws/$file" ]] || continue
        cp -- "$ws/$file" "$dest/$file"
        [[ "$file" == mise.lock ]] || config=true
    done
    [[ $config == true ]] || return 0
    if [[ -f "$dest/mise.lock" ]]; then TOOL_CONFIG=locked; else TOOL_CONFIG=unlocked; fi
}

# The script the install container runs: install the locked tools, download
# the Gradle distribution, copy the licenses.
# Argument: the agent.
#
# Gradle distributions redirect to GitHub, which agents can't reach, so the
# project's wrapper downloads its distribution here, with a mise JDK and
# GRADLE_USER_HOME on the agent's Gradle volume; the wrapper verifies
# distributionSha256Sum itself. The wrapper jar is the project's, so it runs
# only if it matches the checksum Gradle publishes for that version.
TOOLS_INSTALL_SCRIPT='
set -euo pipefail
skip() { echo "agent-run: Gradle distribution not downloaded: $1" >&2; }
if ! command -v mise >/dev/null; then
    echo "agent-run: this image has no mise; rebuild it: podman compose build $1" >&2
    exit 1
fi
if [ -f /project/mise.toml ] || [ -f /project/.mise.toml ]; then
    cd /project
    mise install --locked
    mise reshim
fi
if [ -f /project/gradle-wrapper.properties ]; then
    props=/project/gradle-wrapper.properties
    url="$(sed -n "s/^distributionUrl=//p" "$props" | sed "s/\\\\:/:/g")"
    version="$(printf "%s" "$url" | sed -n "s|.*/gradle-\\(.*\\)-[a-z]*\\.zip$|\\1|p")"
    java="$(cd /project && mise which java 2>/dev/null || true)"
    if ! grep -q "^distributionSha256Sum=." "$props"; then
        skip "gradle-wrapper.properties has no distributionSha256Sum"
    elif [ ! -f /project/gradle-wrapper.jar ]; then
        skip "no gradle/wrapper/gradle-wrapper.jar"
    elif [ -z "$version" ]; then
        skip "unrecognized distributionUrl: $url"
    elif [ -z "$java" ]; then
        skip "no Java in the project'"'"'s mise config"
    else
        official="$(curl -fsSL "https://services.gradle.org/distributions/gradle-$version-wrapper.jar.sha256")"
        actual="$(sha256sum /project/gradle-wrapper.jar | cut -d " " -f 1)"
        if [ "$official" != "$actual" ]; then
            skip "gradle-wrapper.jar does not match Gradle'"'"'s checksum for $version; on the host run: ./gradlew wrapper"
        else
            mkdir /tmp/wrapper
            cp /project/gradle-wrapper.jar "$props" /tmp/wrapper/
            (cd /tmp/wrapper && GRADLE_USER_HOME=/gradle "$java" -cp gradle-wrapper.jar \
                org.gradle.wrapper.GradleWrapperMain --version >/dev/null)
        fi
    fi
fi
if [ -d /licenses ]; then
    mkdir -p /android/licenses
    cp /licenses/* /android/licenses/
fi
'

# install_tools IMAGE AGENT CONFIG_DIR LICENSES GRADLE_HOME: runs the install
# container (see above). LICENSES is a host folder of accepted Android SDK
# licenses, or empty. GRADLE_HOME is the volume of the agent's Gradle home for
# the workspace. Fails if the install fails.
install_tools() {
    local image="$1" agent="$2" config="$3" licenses="$4" gradle_home="$5" volume
    for volume in "agent-run-$agent-mise" "agent-run-$agent-android" "$gradle_home"; do
        podman volume exists "$volume" || podman volume create "$volume" >/dev/null ||
            die "could not create volume $volume"
    done
    local mounts=(-v "agent-run-$agent-mise:/mise" -v "agent-run-$agent-android:/android"
        -v "$gradle_home:/gradle" -v "$config:/project:ro,z")
    [[ -z "$licenses" ]] || mounts+=(-v "$licenses:/licenses:ro,z")
    podman run --rm --network agent-run_tools --read-only --cap-drop ALL \
        --security-opt no-new-privileges --userns keep-id:uid=1000,gid=1000 \
        -e HTTPS_PROXY=http://proxy:3128 -e https_proxy=http://proxy:3128 \
        -e JAVA_TOOL_OPTIONS="$JAVA_PROXY_OPTIONS" \
        -e MISE_TRUSTED_CONFIG_PATHS=/project -e MISE_YES=1 \
        "${mounts[@]}" --entrypoint bash "$image" -c "$TOOLS_INSTALL_SCRIPT" install "$agent" </dev/null
}

# tool_mounts AGENT GRADLE_HOME: sets TOOL_MOUNTS to the agent container's
# mounts of the three volumes, as `podman run` arguments in one string; the
# tools are read-only. They go through --podman-run-args, because
# podman-compose prefixes the names of volumes given to `run -v` with the
# project name.
tool_mounts() {
    TOOL_MOUNTS="--mount type=volume,src=agent-run-$1-mise,dst=/mise,ro=true"
    TOOL_MOUNTS="$TOOL_MOUNTS --mount type=volume,src=agent-run-$1-android,dst=/android"
    TOOL_MOUNTS="$TOOL_MOUNTS --mount type=volume,src=$2,dst=/home/node/.gradle"
}
