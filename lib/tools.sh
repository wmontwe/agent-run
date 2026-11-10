# Build toolchains. A project's tools (its mise.toml, pinned with checksums in
# mise.lock) are installed by `mise install --locked` in a throwaway container
# before the agent starts, into the per-agent volume agent-run-<agent>-mise,
# which the agent gets read-only at /mise. mise runs commands from its config
# (hooks, exec templates), and agents can edit it, so only a config the user
# approved with `agent trust-tools` is installed. That container sees only
# copies of the project's mise files, never the workspace, and downloads
# through proxy-tools; agents never reach those hosts (GitHub among them). The
# same container copies the user's accepted Android SDK licenses into
# agent-run-<agent>-android (/android; the Android Gradle Plugin downloads SDK
# packages itself), and downloads the Gradle distribution the project's wrapper
# names into agent-run-<agent>-dists, which no agent mounts. A second container,
# without network, copies it into the agent's Gradle home for the workspace
# (~/.gradle, lib/gradle.sh). Java, Kotlin and Gradle settings are the
# project's; nothing here configures them.

# copy_tool_config WS DEST: copies the project's mise files and Gradle wrapper
# properties, those that are plain files, into DEST. Sets TOOL_CONFIG to
# "locked" (a mise config and mise.lock), "unlocked" (a config without lock)
# or "none".
copy_tool_config() {
    local ws="$1" dest="$2" file config=false
    TOOL_CONFIG=none
    mkdir -p -- "$dest"
    if [[ ! -L "$ws/gradle" && ! -L "$ws/gradle/wrapper" ]]; then
        file="$ws/gradle/wrapper/gradle-wrapper.properties"
        [[ ! -f "$file" || -L "$file" ]] || cp -- "$file" "$dest/gradle-wrapper.properties"
    fi
    for file in mise.toml .mise.toml mise.lock; do
        [[ -f "$ws/$file" && ! -L "$ws/$file" ]] || continue
        cp -- "$ws/$file" "$dest/$file"
        [[ "$file" == mise.lock ]] || config=true
    done
    [[ $config == true ]] || return 0
    if [[ -f "$dest/mise.lock" ]]; then TOOL_CONFIG=locked; else TOOL_CONFIG=unlocked; fi
}

# tool_config_digest DIR: prints a digest of the mise files in DIR, as
# copy_tool_config leaves them, names included.
tool_config_digest() {
    local file
    for file in mise.toml .mise.toml mise.lock; do
        if [[ -f "$1/$file" ]]; then
            printf '%s %s\n' "$file" "$(shasum -a 256 <"$1/$file" | cut -c1-64)"
        fi
    done | shasum -a 256 | cut -c1-64
}

# tool_config_trusted DIR RECORD: true if RECORD, written by
# trust_tool_config, approves the mise files in DIR.
tool_config_trusted() {
    local recorded=""
    [[ -f "$2" && ! -L "$2" ]] || return 1
    read -r recorded _ <"$2" || true
    [[ -n "$recorded" && "$recorded" == "$(tool_config_digest "$1")" ]]
}

# trust_tool_config DIR RECORD WS: records the mise files in DIR (copied from
# WS) as approved by the user.
trust_tool_config() {
    mkdir -p -- "${2%/*}" || die "could not create ${2%/*}"
    printf '%s %s\n' "$(tool_config_digest "$1")" "$3" >"$2" || die "could not write $2"
}

# Shell functions for both containers: the Gradle distribution the project's
# wrapper names, if it is one of Gradle's own. Only those are installed, so
# nothing in the wrapper properties can choose what gets downloaded or where.
GRADLE_DIST_FUNCTIONS='
# gradle_dist_url PROPS: the last distributionUrl in PROPS, as Java reads it.
gradle_dist_url() {
    sed -n "s/^distributionUrl=//p" "$1" | sed "s/\\\\:/:/g" | tr -d "\r" | tail -n 1
}
# gradle_dist_name URL: gradle-<version>-bin or -all if URL is a distribution
# on services.gradle.org, else nothing.
gradle_dist_name() {
    local re="^https://services\\.gradle\\.org/distributions/(gradle-[0-9A-Za-z.+-]+-(bin|all))\\.zip$"
    if [[ $1 =~ $re ]]; then
        printf "%s" "${BASH_REMATCH[1]}"
    fi
}
'

# The script the install container runs: install the locked tools, download
# the Gradle distribution, copy the licenses. Argument: the agent.
#
# The distribution is checked against the checksum Gradle publishes next to
# it and unpacked where the wrapper looks for it (dists/<name>/<hash of the
# URL>/, with a .ok marker), so the wrapper never downloads it. Nothing from
# the project runs: no wrapper jar, no Java.
TOOLS_INSTALL_SCRIPT='
set -euo pipefail
skip() { echo "agent-run: Gradle distribution not installed: $1" >&2; }
if ! command -v mise >/dev/null; then
    echo "agent-run: this image has no mise; rebuild it: podman compose build $1" >&2
    exit 1
fi
if [ -f /project/mise.toml ] || [ -f /project/.mise.toml ]; then
    cd /project
    mise install --locked
    mise reshim
fi
'"$GRADLE_DIST_FUNCTIONS"'
if [ -f /project/gradle-wrapper.properties ]; then
    url="$(gradle_dist_url /project/gradle-wrapper.properties)"
    dist="$(gradle_dist_name "$url")"
    if [ -z "$dist" ]; then
        skip "distributionUrl is not on https://services.gradle.org/distributions/: $url"
    else
        hash="$(node -e "const c = require(\"crypto\"); console.log(BigInt(\"0x\" + c.createHash(\"md5\").update(process.argv[1]).digest(\"hex\")).toString(36))" "$url")"
        if [ ! -f "/dists/$dist/$hash/$dist.zip.ok" ]; then
            rm -rf /dists/.partial
            mkdir -p /dists/.partial/out "/dists/$dist"
            curl -fsSL -o "/dists/.partial/$dist.zip" "$url"
            official="$(curl -fsSL "$url.sha256")"
            actual="$(sha256sum "/dists/.partial/$dist.zip" | cut -d " " -f 1)"
            if [ "$official" != "$actual" ]; then
                skip "$dist does not match the checksum Gradle publishes"
            else
                unzip -q "/dists/.partial/$dist.zip" -d /dists/.partial/out
                : >"/dists/.partial/out/$dist.zip.ok"
                rm -rf "/dists/$dist/$hash"
                mv /dists/.partial/out "/dists/$dist/$hash"
            fi
            rm -rf /dists/.partial
        fi
    fi
fi
if [ -d /licenses ]; then
    mkdir -p /android/licenses
    cp /licenses/* /android/licenses/
fi
'

# The script the copy container runs: copies the project's Gradle
# distribution from the cache into the agent's Gradle home, unless the home
# has one. The home is the agent's, so the container gets nothing else: no
# network, the cache read-only.
GRADLE_COPY_SCRIPT='
set -euo pipefail
'"$GRADLE_DIST_FUNCTIONS"'
[ -f /project/gradle-wrapper.properties ] || exit 0
dist="$(gradle_dist_name "$(gradle_dist_url /project/gradle-wrapper.properties)")"
[ -n "$dist" ] && [ -d "/dists/$dist" ] || exit 0
target="/gradle/wrapper/dists/$dist"
if [ ! -e "$target" ] && [ ! -L "$target" ]; then
    mkdir -p /gradle/wrapper/dists
    rm -rf "$target.partial"
    cp -R "/dists/$dist" "$target.partial"
    mv "$target.partial" "$target"
fi
'

# install_tools IMAGE AGENT CONFIG_DIR LICENSES GRADLE_HOME: runs the install
# container, then the copy container (see above). LICENSES is a host folder of
# accepted Android SDK licenses, or empty. GRADLE_HOME is the volume of the
# agent's Gradle home for the workspace. Fails if either fails.
install_tools() {
    local image="$1" agent="$2" config="$3" licenses="$4" gradle_home="$5" volume
    for volume in "agent-run-$agent-mise" "agent-run-$agent-android" \
        "agent-run-$agent-dists" "$gradle_home"; do
        podman volume exists "$volume" || podman volume create "$volume" >/dev/null ||
            die "could not create volume $volume"
    done
    local mounts=(-v "agent-run-$agent-mise:/mise" -v "agent-run-$agent-android:/android"
        -v "agent-run-$agent-dists:/dists" -v "$config:/project:ro,z")
    [[ -z "$licenses" ]] || mounts+=(-v "$licenses:/licenses:ro,z")
    podman run --rm --network agent-run_tools --read-only --cap-drop ALL \
        --security-opt no-new-privileges --userns keep-id:uid=1000,gid=1000 \
        -e HTTPS_PROXY=http://proxy:3128 -e https_proxy=http://proxy:3128 \
        -e MISE_TRUSTED_CONFIG_PATHS=/project -e MISE_YES=1 \
        "${mounts[@]}" --entrypoint bash "$image" -c "$TOOLS_INSTALL_SCRIPT" install "$agent" </dev/null ||
        return 1
    [[ -f "$config/gradle-wrapper.properties" ]] || return 0
    podman run --rm --network none --read-only --cap-drop ALL \
        --security-opt no-new-privileges --userns keep-id:uid=1000,gid=1000 \
        -v "agent-run-$agent-dists:/dists:ro" -v "$gradle_home:/gradle" -v "$config:/project:ro,z" \
        --entrypoint bash "$image" -c "$GRADLE_COPY_SCRIPT" copy </dev/null
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
