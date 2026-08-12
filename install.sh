#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$script_dir"

(
    cd frontend
    npm ci
    npm run build
)

cargo build --release --locked

install -d "$HOME/bin"
install -m 0755 target/release/blogger "$HOME/bin/blogger"

echo "installed blogger to $HOME/bin/blogger"
