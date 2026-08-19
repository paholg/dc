#!/usr/bin/env bash
#
# Render a VHS tape into docs/src/demos/.
#
#     demos/record.sh demos/demo.tape
#
# vhs runs from the repo root, which is what the `Output` and `Source` paths in
# the tapes are relative to.

set -euo pipefail

tape=$(realpath -e "$1")
cd "$(dirname "${BASH_SOURCE[0]}")/.."

demos/setup-dcex.sh

mkdir -p docs/src/demos
vhs "$tape"
