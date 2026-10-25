#!/bin/bash
set -Eeuo pipefail
# The sge release by hand, as .github/workflows/apt.yml publishes it on a push to
# main: dist/build.sh builds the four forms of one version (Cargo.toml) - the deb,
# the image dbinvent/sge, the rpm and the tarball - then, PUSH=1 (the default), the
# scripts of ../infra_apt, next to this checkout (INFRA_APT), publish them: the deb
# (publish-apt.sh) and the rpm (publish-rpm.sh) into the repository, the tarball
# under its paks/ (publish-pak.sh), the image to Docker Hub (publish-docker.sh).
#
#   REPO_DIR=<apt repository root> GPG_KEY=<key> DOCKER_HOST=ssh://<build host> dist/publish.sh
#   DOCKER_LOCAL=1 in place of DOCKER_HOST: the image built on this host's daemon
#   PUSH=0 ...: build and check all four, publish nothing
SRC="$(cd "$(dirname "$(realpath "$0")")/.." && pwd)"
INFRA_APT="${INFRA_APT:-$(cd "$SRC/.." && pwd)/infra_apt}"
: "${REPO_DIR:?REPO_DIR is required}"
: "${GPG_KEY:?GPG_KEY is required}"
PUSH="${PUSH:-1}"
case "${DOCKER_HOST:-}" in
  ssh://*|tcp://*) ;;
  *) [ "${DOCKER_LOCAL:-0}" = 1 ] || { echo "ERROR: DOCKER_HOST names no build host (ssh:// or tcp://) - DOCKER_LOCAL=1 builds here" >&2; exit 1; } ;;
esac

out="$(mktemp)"; trap 'rm -f "$out"' EXIT
GITHUB_OUTPUT="$out" "$SRC/dist/build.sh"
get() { grep -oP "(?<=^$1=).*" "$out" | tail -1; }
VERSION="$(get version)"; DEB="$(get deb)"; RPM="$(get rpm)"; TGZ="$(get tgz)"; TAGS="$(get docker_tags)"
IMAGE="${IMAGE:-dbinvent/sge}"

[ "$PUSH" = 1 ] || { echo "PUSH=$PUSH: nothing published"; exit 0; }
DEB="$DEB" REPO_DIR="$REPO_DIR" CONF_DIR="$SRC/dist/repo-conf" GPG_KEY="$GPG_KEY" PACKAGE=schema-guard CODENAME=yt-cluster \
  "$INFRA_APT/scripts/publish-apt.sh"
RPM="$RPM" REPO_DIR="$REPO_DIR" GPG_KEY="$GPG_KEY" PACKAGE=schema-guard "$INFRA_APT/scripts/publish-rpm.sh"
TGZ="$TGZ" NAME=sge VERSION="$VERSION" REPO_DIR="$REPO_DIR" GPG_KEY="$GPG_KEY" "$INFRA_APT/scripts/publish-pak.sh"
CONTEXT="$SRC" IMAGE="$IMAGE" TAGS="$TAGS" BUILD=0 README="$SRC/dist/dockerhub.md" \
  DESCRIPTION="Schema Guard: PostgreSQL schema migration, SQL scripts and declarative YAML (MIT)" \
  "$INFRA_APT/scripts/publish-docker.sh"
echo "sge $VERSION published: the deb, the rpm, paks/sge-latest.tar.gz, $IMAGE:{${TAGS// /,}}"
