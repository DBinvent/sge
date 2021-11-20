#!/bin/bash
rustup update && cargo build --release --target x86_64-unknown-linux-musl
