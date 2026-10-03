#!/bin/sh
# Run inside the directory containing the already qualified release archives.
set -eu
: "${RELEASE_TAG:?release tag required}"
sha256sum herdr-tokens-*.tar.gz > SHA256SUMS
if gh release view "$RELEASE_TAG" >/dev/null 2>&1; then
  existing=$(mktemp -d)
  trap 'rm -rf "$existing"' 0
  trap 'exit 1' HUP INT TERM
  gh release download "$RELEASE_TAG" --dir "$existing"
  # A published version is immutable. Missing or different assets require a new version.
  for asset in ./*.tar.gz SHA256SUMS; do
    cmp "$asset" "$existing/$(basename "$asset")" || {
      echo "Published release differs: $(basename "$asset"); publish a new version" >&2
      exit 1
    }
  done
else
  gh release create "$RELEASE_TAG" --verify-tag --title "$RELEASE_TAG" --generate-notes ./*.tar.gz SHA256SUMS
fi
