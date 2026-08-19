# Environment for demo recordings.
#
# Sourced by `demos/tape.bashrc` (inside the recording) and by
# `demos/setup-dcex.sh`, so both agree on where things live.

_demos_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# The example project, reset to latest `main` by `setup-dcex.sh`.
export EXAMPLE_REPO="https://github.com/paholg/dcex"
export EXAMPLE_DIR="/tmp/dcex"

# Record against demos/config.toml rather than your real config.
export DEVCONCURRENT_CONFIG="$_demos_dir/devconcurrent"

# The demo prompt, rather than your real one.
export STARSHIP_CONFIG="$_demos_dir/starship.toml"

# The current build of `devconcurrent`; `record.sh` builds it first.
PATH="$(cargo metadata --manifest-path "$_demos_dir/../Cargo.toml" \
    --format-version=1 --no-deps | jq -r .target_directory)/debug:$PATH"
export PATH

unset _demos_dir
