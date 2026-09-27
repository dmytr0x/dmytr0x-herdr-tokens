#!/bin/sh
set -eu

: "${HERDR_PLUGIN_ROOT:?Herdr did not provide HERDR_PLUGIN_ROOT}"
: "${HERDR_PLUGIN_CONFIG_DIR:?Herdr did not provide HERDR_PLUGIN_CONFIG_DIR}"

config="$HERDR_PLUGIN_CONFIG_DIR/tokens.toml"
if [ ! -e "$config" ]; then
  cp "$HERDR_PLUGIN_ROOT/examples/tokens.toml" "$config"
fi

exec "$HERDR_PLUGIN_ROOT/target/release/herdr-tokens" start
