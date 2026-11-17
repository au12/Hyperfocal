#!/bin/sh
# Applies UMASK, prepares /config and drops root to PUID:PGID before starting
# Hyperfocal. The photo library is never chowned or chmodded: only the app's own
# /config directory is.
set -eu

PUID="${PUID:-99}"
PGID="${PGID:-100}"
UMASK="${UMASK:-000}"
CONFIG="${HYPERFOCAL_CONFIG:-/config}"

umask "$UMASK"

if [ "$(id -u)" = "0" ]; then
  mkdir -p "$CONFIG"
  # Only when it is wrong: first start, or PUID/PGID changed.
  if [ "$(stat -c %u:%g "$CONFIG")" != "$PUID:$PGID" ]; then
    echo "entrypoint: giving $CONFIG to $PUID:$PGID"
    chown -R "$PUID:$PGID" "$CONFIG" || echo "entrypoint: warning: could not chown $CONFIG" >&2
  fi
  exec setpriv --reuid="$PUID" --regid="$PGID" --clear-groups --inh-caps=-all -- "$@"
fi

# Started with --user: run as that user.
exec "$@"
