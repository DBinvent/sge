#!/bin/bash
set -Eeu

rustup update
#cargo +stable build --release --target x86_64-unknown-linux-musl --bin rdbm --features=tls
#mv target/release/rdbm target/release/rdbmtls
cargo build --release --target x86_64-unknown-linux-musl
