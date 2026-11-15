#!/bin/bash
# Update Vanta Explorer: the newest version from GitHub, built and installed.
#
# makepkg writes the version it builds into PKGBUILD (that is how a -git package gets its
# version number), so after the first build git sees PKGBUILD as changed and will not pull.
# That file is put back first. Any other change in this folder is left alone, and git stops
# on it rather than overwrite it.
set -euo pipefail
cd "$(dirname "$(readlink -f "$0")")"
git checkout -- PKGBUILD
git pull --ff-only
makepkg -si "$@"
