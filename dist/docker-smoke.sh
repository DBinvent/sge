#!/bin/bash
# dbinvent/sge's check before a push: the image runs sge of the version it should
# be (SGE_VERSION), and the psql sge applies SQL files with
set -uo pipefail
IMG="${1:?image}"
v="$(docker run --rm "$IMG" --version 2>&1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)"
[ -z "${SGE_VERSION:-}" ] || [ "$v" = "$SGE_VERSION" ] || { echo "  FAIL  $IMG: sge ${v:-did not run}, not $SGE_VERSION"; exit 1; }
echo "  OK    $IMG: sge $v"
p="$(docker run --rm --entrypoint psql "$IMG" --version 2>&1)" || { echo "  FAIL  $IMG: psql: $p"; exit 1; }
echo "  OK    $IMG: $p"
