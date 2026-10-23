#!/usr/bin/env bash
# Builds x2d, installs the page and the server where geiger-x2d runs them,
# and restarts it.
set -euo pipefail
cd "$(dirname "$0")"
cargo test --quiet
(cd client && trunk build --release)
cargo build --release --quiet -p x2d-server
app="$HOME/server/apps/x2d"
mkdir -p "$app/site"
rsync -a --delete client/dist/ "$app/site/"
install -D target/release/x2d "$app/x2d"
systemctl --user restart geiger-x2d
