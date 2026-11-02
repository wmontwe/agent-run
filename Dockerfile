# Pinned by digest (the multi-arch index), so a rebuild never silently picks
# up a changed base image. Dependabot keeps the tag and digest up to date.
FROM docker.io/library/node:25-bookworm-slim@sha256:81db02c4b671288a03915da9534dbd54f96d0e7c24d80ccc54f5b36b2e684370

RUN apt-get update \
    && apt-get install -y --no-install-recommends bash ca-certificates curl fd-find git ripgrep \
    && ln -s /usr/bin/fdfind /usr/local/bin/fd \
    && rm -rf /var/lib/apt/lists/*

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
