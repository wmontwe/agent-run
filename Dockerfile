# Pinned by digest (the multi-arch index), so a rebuild never silently picks
# up a changed base image. Dependabot keeps the tag and digest up to date.
FROM docker.io/library/node:25-bookworm-slim@sha256:81db02c4b671288a03915da9534dbd54f96d0e7c24d80ccc54f5b36b2e684370

RUN apt-get update \
    && apt-get install -y --no-install-recommends bash ca-certificates curl fd-find git ripgrep unzip \
    && ln -s /usr/bin/fdfind /usr/local/bin/fd \
    && rm -rf /var/lib/apt/lists/*

# Android's aapt2 is published for Linux on x86-64 only. On ARM64 the Podman
# VM runs it through QEMU (binfmt), which needs its x86-64 runtime libraries;
# everything else, the JVM included, stays native.
RUN if [ "$(uname -m)" = aarch64 ]; then \
      dpkg --add-architecture amd64 \
      && apt-get update \
      && apt-get install -y --no-install-recommends libc6:amd64 libstdc++6:amd64 zlib1g:amd64 \
      && rm -rf /var/lib/apt/lists/*; \
    fi

# mise, pinned by version and checksum (Dependabot can't update these; bump by
# hand from the release's SHASUMS256.txt). bin/agent installs a project's
# tools with it in a separate container into a per-agent volume at /mise,
# which agent containers get read-only; the shims put the tools on PATH, and
# nothing installs at run time. /android and ~/.gradle are per-agent volumes too.
ARG MISE_VERSION=2026.9.12
ARG MISE_SHA256_ARM64=e4a0921da0a76ce4666832d5b57b6f0eb9f22d149ba92845ebae4c38638c6775
ARG MISE_SHA256_X64=b4058dece685259910d3aba5782445996eea79dbdb3cf952a6eb81aadf0373ff
RUN case "$(uname -m)" in \
      aarch64) arch=arm64 sum="$MISE_SHA256_ARM64" ;; \
      x86_64) arch=x64 sum="$MISE_SHA256_X64" ;; \
      *) exit 1 ;; \
    esac \
    && curl -fsSLo /tmp/mise.tar.gz \
      "https://github.com/jdx/mise/releases/download/v$MISE_VERSION/mise-v$MISE_VERSION-linux-$arch.tar.gz" \
    && echo "$sum  /tmp/mise.tar.gz" | sha256sum -c - \
    && tar -xzf /tmp/mise.tar.gz -C /tmp \
    && install -m 755 /tmp/mise/bin/mise /usr/local/bin/mise \
    && rm -rf /tmp/mise /tmp/mise.tar.gz \
    && mkdir -p /mise /android \
    && chown node:node /mise /android
ENV MISE_DATA_DIR=/mise \
    MISE_CACHE_DIR=/tmp/mise-cache \
    MISE_STATE_DIR=/tmp/mise-state \
    MISE_TRUSTED_CONFIG_PATHS=/workspace \
    MISE_NOT_FOUND_AUTO_INSTALL=false \
    MISE_EXEC_AUTO_INSTALL=false \
    MISE_TASK_RUN_AUTO_INSTALL=false \
    MISE_HIDE_UPDATE_WARNING=1 \
    ANDROID_HOME=/android \
    PATH=/mise/shims:$PATH

# One agent per image, so agents never share binaries or config. Each agent's
# CLI and every dependency are pinned with checksums in
# agents/<agent>/package-lock.json; npm ci installs exactly that. Install
# scripts never run: they would execute arbitrary package code at build time.
ARG AGENT
COPY agents/${AGENT}/package.json agents/${AGENT}/package-lock.json /opt/agent/
RUN cd /opt/agent \
    && npm ci --ignore-scripts --no-audit --no-fund \
    # Workaround until Claude Code installs without its postinstall script
    # (i.e. supports --ignore-scripts): the skipped script only moves the
    # platform binary from its optional dependency over the bin/claude.exe
    # placeholder, so do that here. Remove this block once it's no longer needed.
    && if [ "$AGENT" = claude ]; then \
      case "$(uname -m)" in x86_64) arch=x64 ;; aarch64) arch=arm64 ;; *) exit 1 ;; esac; \
      mods=/opt/agent/node_modules/@anthropic-ai; \
      mv "$mods/claude-code-linux-$arch/claude" "$mods/claude-code/bin/claude.exe" \
      && chmod 755 "$mods/claude-code/bin/claude.exe"; \
    fi \
    && npm cache clean --force

ENV PATH=/opt/agent/node_modules/.bin:$PATH
ENV BROWSER=echo
USER node
WORKDIR /workspace
CMD ["bash"]
