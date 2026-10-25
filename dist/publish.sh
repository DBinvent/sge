#!/bin/bash
set -Eeuo pipefail
# The sge release in its three forms, one version (Cargo.toml):
#   1. the image dbinvent/sge - <version>, <major.minor>, latest - built from this
#      repository's Dockerfile (a static musl sge, and psql) and checked;
#   2. the tarball: that image's own sge binary, laid out for /usr/local
#      (sudo tar -xzf sge-latest.tar.gz -C /usr/local);
#   3. the deb (dist/build.sh: the unit tests, then the package) - into the apt
#      repository, unless it lists this version already.
# Then, PUSH=1 (the default), all three are published: the deb (publish-apt.sh),
# the tarball under paks/ of the same repository (publish-pak.sh), the image to
# Docker Hub (publish-docker.sh) - the scripts of ../infra_apt, next to this
# checkout (INFRA_APT).
#
#   REPO_DIR=<apt repository root> GPG_KEY=<key> DOCKER_HOST=ssh://<build host> dist/publish.sh
#   PUSH=0 ...: build and check all three, publish nothing
SRC="$(cd "$(dirname "$(realpath "$0")")/.." && pwd)"
INFRA_APT="${INFRA_APT:-$(cd "$SRC/.." && pwd)/infra_apt}"
: "${REPO_DIR:?REPO_DIR is required}"
: "${GPG_KEY:?GPG_KEY is required}"
PUSH="${PUSH:-1}"
VERSION="$(grep -m1 -oP '(?<=^version = ")[^"]+' "$SRC/Cargo.toml")"
IMAGE="${IMAGE:-dbinvent/sge}"
TAGS="$VERSION ${VERSION%.*} latest"
echo "sge $VERSION: the image, the tarball, the deb"

# 1. the image
SGE_VERSION="$VERSION" CONTEXT="$SRC" IMAGE="$IMAGE" TAGS="$TAGS" BUILD_ARGS="SGE_VERSION=$VERSION" \
  SMOKE="$SRC/dist/docker-smoke.sh" PUSH=0 "$INFRA_APT/scripts/publish-docker.sh"

# 2. the tarball, from the image's binary
STAGE="$(mktemp -d)"; trap 'rm -rf "$STAGE"' EXIT
install -d "$STAGE/bin" "$STAGE/share/doc/schema-guard"
cid="$(docker create "$IMAGE:$VERSION")"
docker cp "$cid:/usr/local/bin/sge" "$STAGE/bin/sge"; docker rm "$cid" >/dev/null
file "$STAGE/bin/sge" | grep -q 'static' || { echo "ERROR: the image's sge is not a static binary" >&2; exit 1; }
install -m 644 "$SRC/LICENSE" "$SRC/COPYRIGHT" "$SRC/lib-licenses.txt" "$STAGE/share/doc/schema-guard/"
mkdir -p "$SRC/target/tgz"
TGZ="$SRC/target/tgz/sge-$VERSION-x86_64-linux.tar.gz"

# 3. the deb, and its man page for the tarball too
published="$(reprepro -b "$REPO_DIR" list yt-cluster schema-guard 2>/dev/null | awk '{print $NF; exit}')"
DEB=""
if [ "${published%-*}" = "$VERSION" ]; then
  echo "  OK    the deb: the repository lists schema-guard $published already - not built again"
  # the man pages as dist/build.sh writes them, from the tarball's own binary
  "$SRC/man/build.sh" "$STAGE/bin/sge" "$STAGE/share/man"
else
  out="$(mktemp)"
  GITHUB_OUTPUT="$out" "$SRC/dist/build.sh"
  DEB="$(grep -oP '(?<=^deb=).*' "$out" | tail -1)"; rm -f "$out"
  [ -f "$DEB" ] || { echo "ERROR: dist/build.sh named no deb" >&2; exit 1; }
  install -d "$STAGE/share/man"
  cp -r "$SRC/target/man/." "$STAGE/share/man/"
fi
tar --sort=name --owner=0 --group=0 --numeric-owner -C "$STAGE" -czf "$TGZ" bin share
echo "  OK    the tarball: $TGZ ($(du -h "$TGZ" | cut -f1)): $(tar -tzf "$TGZ" | grep -v '/$' | tr '\n' ' ')"

[ "$PUSH" = 1 ] || { echo "PUSH=$PUSH: nothing published"; exit 0; }
if [ -n "$DEB" ]; then
  DEB="$DEB" REPO_DIR="$REPO_DIR" CONF_DIR="$SRC/dist/repo-conf" GPG_KEY="$GPG_KEY" PACKAGE=schema-guard CODENAME=yt-cluster \
    "$INFRA_APT/scripts/publish-apt.sh"
fi
TGZ="$TGZ" NAME=sge VERSION="$VERSION" REPO_DIR="$REPO_DIR" GPG_KEY="$GPG_KEY" "$INFRA_APT/scripts/publish-pak.sh"
CONTEXT="$SRC" IMAGE="$IMAGE" TAGS="$TAGS" BUILD=0 README="$SRC/dist/dockerhub.md" \
  DESCRIPTION="Schema Guard: PostgreSQL schema migration, SQL scripts and declarative YAML (MIT)" \
  "$INFRA_APT/scripts/publish-docker.sh"
echo "sge $VERSION published: $( [ -n "$DEB" ] && echo "the deb, ")paks/sge-latest.tar.gz, $IMAGE:{${TAGS// /,}}"
