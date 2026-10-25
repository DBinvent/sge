#!/bin/bash
set -Eeu
# Builds the schema-guard .deb (the sge binary; cargo-deb names the package after
# the crate, schema_guard, with its "_" as "-") and writes `deb=<abs path>` to
# $GITHUB_OUTPUT, which is the
# contract ../infra_apt's reusable workflow expects from every calling project (see
# its README, "Onboarding a new project").
#
# Not ../build.sh: that one is the old release build - a musl binary in docker, an
# rpm, and sudo. The runner host builds no docker images, so this packages the
# glibc binary as a .deb, nothing else.
START_TIME=$(date +%s)

SCRIPT_DIR=$(dirname "$(realpath "$0")")
SRC_DIR=$(realpath "$SCRIPT_DIR/..")
if [ ! -f "$SRC_DIR/src/main.rs" ] || ! grep -q '^name = "schema_guard"' "$SRC_DIR/Cargo.toml"; then
  echo "ERROR: $SRC_DIR does not look like sge" >&2
  exit 1
fi

if ! cargo deb --version >/dev/null 2>&1; then
  echo "ERROR: cargo-deb is not installed. Install it with:" >&2
  echo "    cargo install cargo-deb" >&2
  exit 1
fi
# the package ships the man pages ([package.metadata.deb] assets), and help2man is
# what writes them
if ! command -v help2man >/dev/null 2>&1; then
  echo "ERROR: help2man is not installed: sudo apt install help2man" >&2
  exit 1
fi

VERSION=$(grep -m1 -oP '(?<=^version = ")[^"]+' "$SRC_DIR/Cargo.toml")
echo "Building sge $VERSION from $SRC_DIR ..."

cd "$SRC_DIR"

# unit tests only: the integration tests start a Postgres of their own
cargo test --lib --bins
cargo build --release

# the man pages: `sge --help` in English and in each of its translations, with the
# NAME and EXAMPLES of man/<lang>.h2m, into target/man - see man/build.sh
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

if [ -n "${GITHUB_OUTPUT:-}" ]; then
  echo "deb=$DEB" >> "$GITHUB_OUTPUT"
fi

ELAPSED_MIN=$(( ($(date +%s) - START_TIME) / 60 ))
echo "Build done in ${ELAPSED_MIN} min: $DEB"
