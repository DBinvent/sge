#!/bin/bash
set -Eeu
# Builds the sge release in its four forms, one version (Cargo.toml), and writes
# what ../infra_apt's reusable workflow (v5) publishes to $GITHUB_OUTPUT (see its
# README, "Onboarding a new project"):
#   deb=<abs path>      the schema-guard .deb: the glibc sge (cargo-deb names the
#                       package after the crate, schema_guard, with its "_" as "-")
#   docker_tags=<tags>  the image dbinvent/sge (IMAGE) - <version>, <major.minor>,
#                       latest - from this repository's Dockerfile (a static musl sge,
#                       and psql), checked by dist/docker-smoke.sh
#   rpm=<abs path>      the schema-guard .rpm: the image's static sge
#   tgz=<abs path>      the tarball: the image's static sge, laid out for /usr/local
#                       (sudo tar -xzf sge-latest.tar.gz -C /usr/local)
#   version=<x.y.z>
# The image is built on DOCKER_HOST (ssh:// or tcp://: lab10, also from the GitHub
# action, whose apt.yml names it) - this host's own daemon only with DOCKER_LOCAL=1,
# as dist/publish.sh and infra_apt's publish-docker.sh take it. With buildx, its SBOM
# and provenance attestations kept with it, which needs that daemon's containerd
# image store: the push carries them to Docker Hub.
# The SBOM (CycloneDX, target/sbom/sbom.cdx.json) is the image's: the Dockerfile
# writes it, and the deb, rpm and tarball carry the same file.
#
# Not ../build.sh: that one is the old release build - and sudo.
START_TIME=$(date +%s)

SCRIPT_DIR=$(dirname "$(realpath "$0")")
SRC_DIR=$(realpath "$SCRIPT_DIR/..")
if [ ! -f "$SRC_DIR/src/main.rs" ] || ! grep -q '^name = "schema_guard"' "$SRC_DIR/Cargo.toml"; then
  echo "ERROR: $SRC_DIR does not look like sge" >&2
  exit 1
fi

for t in deb generate-rpm; do
  if ! cargo "$t" --version >/dev/null 2>&1; then
    echo "ERROR: cargo-$t is not installed. Install it with:" >&2
    echo "    cargo install cargo-$t" >&2
    exit 1
  fi
done
# the packages ship the man pages ([package.metadata.deb] assets), and help2man is
# what writes them
if ! command -v help2man >/dev/null 2>&1; then
  echo "ERROR: help2man is not installed: sudo apt install help2man" >&2
  exit 1
fi
for c in docker file; do
  command -v "$c" >/dev/null 2>&1 || { echo "ERROR: $c is not installed" >&2; exit 1; }
done
# docker is lab10's: an empty DOCKER_HOST - a runner's service has no login profile -
# would build on this host's daemon unnoticed
case "${DOCKER_HOST:-}" in
  ssh://*|tcp://*) ;;
  *) [ "${DOCKER_LOCAL:-0}" = 1 ] || { echo "ERROR: DOCKER_HOST names no build host (ssh:// or tcp://) - DOCKER_LOCAL=1 builds here" >&2; exit 1; } ;;
esac
if ! docker buildx version >/dev/null 2>&1; then
  echo "ERROR: docker buildx is not installed: sudo apt install docker-buildx (Ubuntu's docker.io;" >&2
  echo "with Docker's docker-ce: docker-buildx-plugin)" >&2
  exit 1
fi
# attestations are kept only in the containerd image store (the docker driver's)
if ! docker info -f '{{.DriverStatus}}' 2>/dev/null | grep -q 'io.containerd.snapshotter'; then
  echo "ERROR: ${DOCKER_HOST:-this host}'s docker has no containerd image store, which the" >&2
  echo "image's attestations need: \"features\": {\"containerd-snapshotter\": true} in" >&2
  echo "/etc/docker/daemon.json, and restart docker" >&2
  exit 1
fi

VERSION=$(grep -m1 -oP '(?<=^version = ")[^"]+' "$SRC_DIR/Cargo.toml")
IMAGE="${IMAGE:-dbinvent/sge}"
TAGS="$VERSION ${VERSION%.*} latest"
echo "Building sge $VERSION from $SRC_DIR: the deb, the image $IMAGE, the rpm, the tarball ..."

cd "$SRC_DIR"

# 1. the glibc sge the deb carries. Unit tests only: the integration tests start a
# Postgres of their own
cargo test --lib --bins
cargo build --release

# the man pages: `sge --help` in English and in each of its translations, with the
# NAME and EXAMPLES of man/<lang>.h2m, into target/man - see man/build.sh; the rpm
# and the tarball carry them too
man/build.sh target/release/sge target/man

# 2. the image, under every tag, and its check. Its SBOM attestation scans the image
# (Alpine's packages, and the crates through its sbom.cdx.json); provenance: how and
# from what it was built
build_args=(--pull --build-arg "SGE_VERSION=$VERSION" --sbom=true --provenance=mode=max)
for t in $TAGS; do build_args+=(-t "$IMAGE:$t"); done
echo "Building $IMAGE ($TAGS) on ${DOCKER_HOST:-this host} ..."
docker buildx build "${build_args[@]}" "$SRC_DIR"
SGE_VERSION="$VERSION" "$SCRIPT_DIR/docker-smoke.sh" "$IMAGE:$VERSION"

# the image's own sge: the static binary the rpm ([package.metadata.generate-rpm])
# and the tarball carry
MUSL="$SRC_DIR/target/x86_64-unknown-linux-musl/release/sge"
mkdir -p "$(dirname "$MUSL")"
cid="$(docker create "$IMAGE:$VERSION")"
# and its SBOM, for the packages ([package.metadata.deb] and generate-rpm assets)
SBOM="$SRC_DIR/target/sbom/sbom.cdx.json"
mkdir -p "$(dirname "$SBOM")"
docker cp "$cid:/usr/local/bin/sge" "$MUSL"
docker cp "$cid:/usr/share/doc/schema-guard/sbom.cdx.json" "$SBOM"; docker rm "$cid" >/dev/null
file "$MUSL" | grep -q 'static' || { echo "ERROR: the image's sge is not a static binary" >&2; exit 1; }
grep -q '"bomFormat": *"CycloneDX"' "$SBOM" || { echo "ERROR: the image's sbom.cdx.json is no CycloneDX SBOM" >&2; exit 1; }

# 3. the deb, now the SBOM is there.
# --no-build: package exactly the binary tested and built above
cargo deb --no-build

DEB="$SRC_DIR/target/debian/schema-guard_${VERSION}-1_amd64.deb"
if [ ! -f "$DEB" ]; then
  # cargo-deb's revision suffix has changed across versions; fall back to a glob,
  # but insist on exactly one match so an old package is never published by accident
  mapfile -t CANDIDATES < <(ls -1 "$SRC_DIR"/target/debian/schema-guard_"${VERSION}"*_amd64.deb 2>/dev/null || true)
  if [ "${#CANDIDATES[@]}" -ne 1 ]; then
    echo "ERROR: expected exactly one schema-guard $VERSION .deb in $SRC_DIR/target/debian," >&2
    echo "found ${#CANDIDATES[@]}: ${CANDIDATES[*]-none}" >&2
    exit 1
  fi
  DEB="${CANDIDATES[0]}"
fi

# 4. the rpm. -o is a directory only when it exists
rm -rf target/generate-rpm; mkdir -p target/generate-rpm
cargo generate-rpm -o target/generate-rpm/
mapfile -t RPMS < <(ls -1 "$SRC_DIR"/target/generate-rpm/schema-guard-"${VERSION}"-*.rpm 2>/dev/null || true)
[ "${#RPMS[@]}" -eq 1 ] || { echo "ERROR: expected exactly one schema-guard $VERSION .rpm, found ${#RPMS[@]}" >&2; exit 1; }
RPM="${RPMS[0]}"

# 5. the tarball
STAGE="$(mktemp -d)"; trap 'rm -rf "$STAGE"' EXIT
install -D -m 755 "$MUSL" "$STAGE/bin/sge"
install -d "$STAGE/share/doc/schema-guard" "$STAGE/share/man"
install -m 644 LICENSE COPYRIGHT lib-licenses.txt "$SBOM" "$STAGE/share/doc/schema-guard/"
cp -r target/man/. "$STAGE/share/man/"
mkdir -p target/tgz
TGZ="$SRC_DIR/target/tgz/sge-$VERSION-x86_64-linux.tar.gz"
tar --sort=name --owner=0 --group=0 --numeric-owner -C "$STAGE" -czf "$TGZ" bin share

if [ -n "${GITHUB_OUTPUT:-}" ]; then
  {
    echo "deb=$DEB"
    echo "rpm=$RPM"
    echo "tgz=$TGZ"
    echo "version=$VERSION"
    echo "docker_tags=$TAGS"
  } >> "$GITHUB_OUTPUT"
fi

ELAPSED_MIN=$(( ($(date +%s) - START_TIME) / 60 ))
echo "Build done in ${ELAPSED_MIN} min:"
echo "  $DEB"
echo "  $RPM"
echo "  $TGZ: $(tar -tzf "$TGZ" | grep -v '/$' | tr '\n' ' ')"
echo "  $IMAGE:{${TAGS// /,}}"
