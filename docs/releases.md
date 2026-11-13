# Publishing signed images

The release workflow publishes a coordinated Linux ARM64 image set to GHCR. The launcher
currently builds images locally. Automatic download and verification in the launcher is
separate work.

## Repository setup

Before creating the first release:

1. Protect `main` and release tags matching `v*` with repository rulesets. Restrict tag
   creation, modification and deletion to release maintainers.
2. Create a GitHub environment named `release`. Restrict deployments to release tags and
   require approval after the local release checks in [AGENTS.md](../AGENTS.md).
3. Allow GitHub Actions to publish packages using `GITHUB_TOKEN`. After first publication,
   make the GHCR packages public so users can pull and verify them without credentials.
4. Protect workflow changes with the same review requirements as launcher code.

Keep release tags fixed once published. Use a new version when the source changes.

## Create a release

Run the local release gate and record the results. Tag the tested commit on `main`, for
example:

```sh
git tag v0.1.0
git push origin v0.1.0
```

Supported tag forms include `v0.1.0` and `v0.1.0-rc.1`. The workflow checks the tag format
and requires its commit to be reachable from `main`.

### Build and test

`build.yml` runs on pull requests and pushes to `main`. `publish-release.yml` reuses it
for release tags and requests OCI archives of the eight tested runtime images. The gate
image remains a test artifact.

Image smoke tests run before export. Build jobs have read-only repository permissions.
PR builds receive no publishing or signing permissions.

### Publish and sign

After release-environment approval, separate jobs download the tested OCI archives and
push them with digest preservation. They sign image digests using Cosign and GitHub OIDC.
The pinned Cosign installer supplies its bundled version and checksums. Dependabot updates
the installer action.

Images are published under:

```text
ghcr.io/wmontwe/agent-run/claude
ghcr.io/wmontwe/agent-run/codex
ghcr.io/wmontwe/agent-run/gemini
ghcr.io/wmontwe/agent-run/pi
ghcr.io/wmontwe/agent-run/build
ghcr.io/wmontwe/agent-run/proxy
ghcr.io/wmontwe/agent-run/github-relay
ghcr.io/wmontwe/agent-run/mirror
```

Each gets the release tag. Consumers should use its signed digest reference.

Once every image is published and signed, the workflow checks that all eight references
belong to the same source commit. It creates `images.json` containing the release,
platform, source commit and image references. Cosign signs this manifest, and both files
are attached to a GitHub Release:

- `images.json`
- `images.json.sigstore.json`

The publishing jobs have package-write and OIDC permissions. The final manifest job has
release-write and OIDC permissions. Signing uses GitHub's short-lived identity without a
stored private key.

## Verify a release manually

Install Cosign, the GitHub CLI and jq. Download release assets into an empty directory:

```sh
version=v0.1.0
gh release download "$version" --repo wmontwe/agent-run \
  --pattern images.json --pattern images.json.sigstore.json
```

Verify the manifest before reading image references from it:

```sh
identity="https://github.com/wmontwe/agent-run/.github/workflows/publish-release.yml@refs/tags/$version"
issuer=https://token.actions.githubusercontent.com

cosign verify-blob \
  --certificate-identity "$identity" \
  --certificate-oidc-issuer "$issuer" \
  --bundle images.json.sigstore.json images.json

jq -e --arg version "$version" \
  '.schema_version == 1 and .version == $version and .platform == "linux/arm64"' images.json
```

Then verify and pull an individual image by digest:

```sh
reference=$(jq -er '.images.claude' images.json)
cosign verify \
  --certificate-identity "$identity" \
  --certificate-oidc-issuer "$issuer" \
  "$reference"
podman pull "$reference"
```

Stop on any verification failure. Keep the exact workflow identity and issuer checks.
Pulling an image this way leaves the current launcher's local-build behavior unchanged.

## Failed releases

A failed build or smoke check prevents publishing. A partial publishing failure prevents
creation of the signed release manifest. Inspect the failing job and rerun the same
workflow for the same fixed tag after resolving registry or runner issues. Published
releases stay fixed. Source corrections get a new tag.

OCI transfer artifacts expire after two days. A later retry may need to rerun the complete
workflow so it has a fresh tested image set. Registry tags can change during an incomplete
attempt, so use the completed release manifest's digests when consuming images.
