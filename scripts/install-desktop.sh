#!/bin/sh
# Served at https://phux.sh/install-desktop: the phux installer with --desktop.
set -eu
installer="$(curl -fsSL --proto =https https://phux.sh/install)" || {
  echo "phux: error: could not download https://phux.sh/install" >&2
  exit 1
}
exec sh -c "$installer" install.sh --desktop "$@"
