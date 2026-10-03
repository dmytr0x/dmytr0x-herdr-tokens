#!/bin/sh
set -eu

repository="https://github.com/dmytr0x/dmytr0x-herdr-tokens"
version=$(awk -F '"' '/^version[[:space:]]*=/ { print $2; exit }' herdr-plugin.toml)

if [ -z "$version" ]; then
  echo "Could not read the plugin version from herdr-plugin.toml" >&2
  exit 1
fi

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target="aarch64-apple-darwin" ;;
  Darwin-x86_64) target="x86_64-apple-darwin" ;;
  Linux-x86_64) target="x86_64-unknown-linux-musl" ;;
  Linux-aarch64|Linux-arm64) target="aarch64-unknown-linux-musl" ;;
  *)
    echo "No prebuilt herdr-tokens binary for $(uname -s)-$(uname -m)" >&2
    exit 1
    ;;
esac

archive="herdr-tokens-${version}-${target}.tar.gz"
release_url="${repository}/releases/download/v${version}"
temp_dir=$(mktemp -d "${TMPDIR:-/tmp}/herdr-tokens.XXXXXX")
staged_binary=""
trap 'rm -rf "$temp_dir"; if [ -n "$staged_binary" ]; then rm -f "$staged_binary"; fi' 0
trap 'exit 1' HUP INT TERM

curl -fsSL "$release_url/$archive" -o "$temp_dir/$archive"
curl -fsSL "$release_url/SHA256SUMS" -o "$temp_dir/SHA256SUMS"

awk -v archive="$archive" '
  $2 == archive { print; found++ }
  END { if (found != 1) exit 1 }
' "$temp_dir/SHA256SUMS" > "$temp_dir/archive.sha256"
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$temp_dir" && sha256sum -c archive.sha256)
elif command -v shasum >/dev/null 2>&1; then
  (cd "$temp_dir" && shasum -a 256 -c archive.sha256)
else
  echo "Installing herdr-tokens requires sha256sum or shasum" >&2
  exit 1
fi

mkdir "$temp_dir/extracted"
tar -xzf "$temp_dir/$archive" -C "$temp_dir/extracted"
test -f "$temp_dir/extracted/herdr-tokens"
test ! -L "$temp_dir/extracted/herdr-tokens"

mkdir -p target/release
staged_binary=$(mktemp target/release/.herdr-tokens.XXXXXX)
install -m 755 "$temp_dir/extracted/herdr-tokens" "$staged_binary"
# Both paths are on the destination filesystem; failed verification/staging leaves
# the installed executable untouched. Stop the runner before invoking this update.
test ! -d target/release/herdr-tokens
mv -f "$staged_binary" target/release/herdr-tokens
staged_binary=""
