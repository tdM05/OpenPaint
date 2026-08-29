#!/usr/bin/env bash
#
# Build and launch OpenPaint locally.
#
# Works from any directory, and on both the Linux dev box and the Windows/tablet
# box (Git Bash), since `$HOME/.cargo/bin` resolves correctly on both.
#
#   ./run.sh              # release build (use this for anything feel-related)
#   ./run.sh --debug      # faster compile, but CPU brush stamping feels sluggish
#   ./run.sh -- --foo     # anything after `--` is passed to the app
#
# The console stays attached on purpose: the pen backend logs its first poses
# (position / pressure / tilt) and every DOWN/UP there, which is how stylus
# behavior gets verified. See docs/DECISIONS.md §8.

set -euo pipefail

# Run from the repo root regardless of where this was invoked from.
cd "$(dirname "${BASH_SOURCE[0]}")"

# rustup installs here on both platforms; harmless if it's already on PATH.
if [ -d "$HOME/.cargo/bin" ]; then
	PATH="$HOME/.cargo/bin:$PATH"
fi

if ! command -v cargo >/dev/null 2>&1; then
	echo "error: cargo not found. Install Rust via https://rustup.rs" >&2
	exit 1
fi

profile=(--release)
if [ "${1-}" = "--debug" ]; then
	profile=()
	shift
fi

# Drop a leading `--` so callers can forward args to the app.
if [ "${1-}" = "--" ]; then
	shift
fi

exec cargo run "${profile[@]}" --bin openpaint -- "$@"
