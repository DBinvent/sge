A builder image for a static (musl) sge binary, adapted from
[emk/rust-musl-builder](https://github.com/emk/rust-musl-builder): Ubuntu with
the Rust toolchain, OpenSSL, zlib and the PostgreSQL client libraries built for
musl.

`build.sh` builds the musl binary with the public `clux/muslrust` image instead;
this one is for building with those libraries pinned.
