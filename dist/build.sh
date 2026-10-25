#!/bin/bash
set -Eeu
# Builds the sge release in its four forms, one version (Cargo.toml), and writes
# what ../infra_apt's reusable workflow (v4) publishes to $GITHUB_OUTPUT (see its
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
# The image is built on DOCKER_HOST when that is set, else on this host's daemon.
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

VERSION=$(grep -m1 -oP '(?<=^version = ")[^"]+' "$SRC_DIR/Cargo.toml")
IMAGE="${IMAGE:-dbinvent/sge}"
TAGS="$VERSION ${VERSION%.*} latest"
echo "Building sge $VERSION from $SRC_DIR: the deb, the image $IMAGE, the rpm, the tarball ..."

cd "$SRC_DIR"

# 1. the deb. Unit tests only: the integration tests start a Postgres of their own
cargo test --lib --bins
cargo build --release

# the man pages: `sge --help` in English and in each of its translations, with the
# NAME and EXAMPLES of man/<lang>.h2m, into target/man - see man/build.sh; the rpm
# and the tarball carry them too
man/build.sh target/release/sge target/man

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

# 2. the image, under every tag, and its check
build_args=(--pull --build-arg "SGE_VERSION=$VERSION")
for t in $TAGS; do build_args+=(-t "$IMAGE:$t"); done
echo "Building $IMAGE ($TAGS) on ${DOCKER_HOST:-this host} ..."
docker build "${build_args[@]}" "$SRC_DIR"
SGE_VERSION="$VERSION" "$SCRIPT_DIR/docker-smoke.sh" "$IMAGE:$VERSION"

# the image's own sge: the static binary the rpm ([package.metadata.generate-rpm])
# and the tarball carry
MUSL="$SRC_DIR/target/x86_64-unknown-linux-musl/release/sge"
mkdir -p "$(dirname "$MUSL")"
cid="$(docker create "$IMAGE:$VERSION")"
docker cp "$cid:/usr/local/bin/sge" "$MUSL"; docker rm "$cid" >/dev/null
file "$MUSL" | grep -q 'static' || { echo "ERROR: the image's sge is not a static binary" >&2; exit 1; }

# 3. the rpm. -o is a directory only when it exists
rm -rf target/generate-rpm; mkdir -p target/generate-rpm
cargo generate-rpm -o target/generate-rpm/
mapfile -t RPMS < <(ls -1 "$SRC_DIR"/target/generate-rpm/schema-guard-"${VERSION}"-*.rpm 2>/dev/null || true)
[ "${#RPMS[@]}" -eq 1 ] || { echo "ERROR: expected exactly one schema-guard $VERSION .rpm, found ${#RPMS[@]}" >&2; exit 1; }
RPM="${RPMS[0]}"

# 4. the tarball
STAGE="$(mktemp -d)"; trap 'rm -rf "$STAGE"' EXIT
install -D -m 755 "$MUSL" "$STAGE/bin/sge"
install -d "$STAGE/share/doc/schema-guard" "$STAGE/share/man"
install -m 644 LICENSE COPYRIGHT lib-licenses.txt "$STAGE/share/doc/schema-guard/"
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
