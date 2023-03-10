#!/bin/bash
set -Eeu

cargo build
target/debug/sl ./keys/corp0.rsa evaluation.license
target/debug/sl ./keys/corp0.rsa tests/signed_test_rdbm.license
cargo test