#!/bin/bash
set -Eeu
echo musl2
#rustup update

#cargo +stable build --release --target x86_64-unknown-linux-musl --bin rdbm --features=tls
#mv target/release/rdbm target/release/rdbmtls
rm /opt/rust/cargo/config
ln -s /home/rust/src/docker/cargo-config-l.toml /opt/rust/cargo/config

cargo build --release --target x86_64-unknown-linux-musl --bin rdbm

#cargo zigbuild --target x86_64-unknown-linux-musl
