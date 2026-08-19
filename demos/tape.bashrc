#!/usr/bin/env bash
# Shell configuration for demo recordings.
#
# `demos/lib/setup.tape` starts the recording shell with `bash --rcfile` on this
# file; `demos/shell.sh` opens the same shell without recording.

source "$(dirname "${BASH_SOURCE[0]}")/env.sh"

# A one-line prompt with the working directory and git branch; configured by
# demos/starship.toml.
eval "$(starship init bash)"

# Don't pollute the real shell history with demo commands.
HISTFILE=/dev/null

# Defines the `dc` function and installs completions for `dc` and
# `devconcurrent`; see docs/src/getting-started/shell-setup.md.
# shellcheck source=/dev/null
source <(COMPLETE=bash devconcurrent) 

cd "$EXAMPLE_DIR" || exit 1
clear
