#!/usr/bin/env bash
#
# Open the recording shell without recording anything, for working out what a
# tape should say.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

demos/setup-dcex.sh

exec bash --rcfile demos/tape.bashrc -i
