#!/usr/bin/env bash
set -euo pipefail

cargo build --release

mkdir -p ~/bin
cp target/release/blogger ~/bin/blogger

echo "installed blogger to ~/bin/blogger"
