#!/bin/sh
set -eu

: "${HERDR_PLUGIN_ROOT:?Herdr did not provide HERDR_PLUGIN_ROOT}"
: "${HERDR_PLUGIN_CONFIG_DIR:?Herdr did not provide HERDR_PLUGIN_CONFIG_DIR}"

config="$HERDR_PLUGIN_CONFIG_DIR/tokens.toml"
# A dangling symlink is an existing user choice, never an invitation to write its target.
if [ -L "$config" ] && [ ! -e "$config" ]; then
  echo "tokens.toml is a dangling symlink; repair it before starting" >&2
  exit 1
fi
if [ ! -e "$config" ]; then
  staged_config=$(mktemp "$HERDR_PLUGIN_CONFIG_DIR/.tokens.toml.XXXXXX")
  trap 'rm -f "$staged_config"' 0
  trap 'exit 1' HUP INT TERM
  cp "$HERDR_PLUGIN_ROOT/examples/tokens.toml" "$staged_config"
  # Hard-link publication is atomic and fails if another start has already won.
  if ! ln "$staged_config" "$config" 2>/dev/null; then
    test -f "$config" || exit 1
  fi
  rm -f "$staged_config"
  trap - 0 HUP INT TERM
fi
test -f "$config" || { echo "tokens.toml must be a regular file" >&2; exit 1; }
exec "$HERDR_PLUGIN_ROOT/target/release/herdr-tokens" start
