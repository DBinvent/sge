#!/bin/bash
rd=$1
BuildId=$2
if [ ${#rd} -lt 1 ]; then
  echo "No target directory set"
  exit 4
fi
if [ $# -ne 2 ]; then
  echo "No build version set"
  exit 4
fi
source $HOME/.cargo/env
echo $HOME
rustup && rustup -V
rustup update
cargo install cargo-make cargo-deb cargo-generate-rpm cross
rm -rf target/*
#sign
cargo +stable build --bin sl && target/debug/sl /etc/corp0.rsa evaluation.license
echo "signed: evaluation.license"
cat evaluation.license
#      cargo test -- -Z unstable-options --format json --report-time | tee test_results_debug.json
cargo +stable test --release
v=$(grep -m 1 version Cargo.toml)
IFS='.' read -r -a va <<< "${v:11:10}"
vv="${va[0]}.${va[1]}.$BuildId"
echo $vv > rdbm.version
sed -i "0,/local_build/s/local_build/$vv/" src/i18n/*_clc.yaml
sed -i "0,/version.*/s//version = \"$vv\"/" Cargo.toml
grep version Cargo.toml
./ul.sh
cargo +stable build --release
strip -s target/release/rdbm
echo "Build: $(./target/release/rdbm -V)"

echo "Building DEB..."
cargo deb
dpkg -I target/debian/*.deb
dpkg --contents  target/debian/*.deb
mt=x86_64-unknown-linux-musl
echo "Rebuild as $mt"
docker run --rm -it -v "$(pwd)":/home/rust/src rust cargo build --release --target $mt
cp -f target/$mt/release/rdbm target/release/.
strip -s target/release/rdbm
cargo generate-rpm
rpm -qiRp target/generate-rpm/*.rpm

mkdir -p $rd
# shellcheck disable=SC2115
rm -rf "$rd"/*
cp target/release/rdbm .
cp target/debian/*.deb .
cp target/generate-rpm/*.rpm .
cp -t . evaluation.license LICENSE lib-licenses.txt deploy.sh repo.sh distro.lst rdbm.version distributions

echo "Prepared to drop: $(ls)"
