#!/bin/bash
set -Eeu
echo musl2
#rustup update

#cargo +stable build --release --target x86_64-unknown-linux-musl --bin sge --features=tls
#mv target/release/sge target/release/sgetls
rm /opt/rust/cargo/config
ln -s /home/rust/src/docker/cargo-config-l.toml /opt/rust/cargo/config

cargo build --release --target x86_64-unknown-linux-musl --bin sge

#cargo zigbuild --target x86_64-unknown-linux-musl
