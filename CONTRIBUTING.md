# Contributing to Schema Guard Enterprise (sge)

Thank you for helping. sge is MIT licensed, with no license key; a contribution is
accepted under the same license (see [LICENSE](LICENSE)).

## Issues

Open an issue on GitHub for a bug or a feature request. For a bug, give the sge
version (`sge --version`), the PostgreSQL version, the command you ran and its
output, and the smallest schema (YAML or SQL) that shows it.

A security problem is not reported in a public issue: see [SECURITY.md](SECURITY.md).

## Pull requests

1. Fork the repository and branch from `main`.
2. Keep a change to one subject; describe what it changes and why.
3. Build and test it (below); add tests for what you change.
4. Messages a user reads go through the locale files (`locales/`): add a new key to
   every one of them.
5. Open the pull request against `main`.

## Build and test

```
cargo build --release      # target/release/sge
cargo test --lib --bins    # the unit tests, no database needed
```

The integration tests (`tests/`) start a throwaway PostgreSQL of their own, so they
need the PostgreSQL server binaries installed; `SGE_IT=ignore` skips them. See
[tests/README.md](tests/README.md).

Before a pull request, run the checks as well:

```
cargo audit      # known vulnerabilities in the pinned dependencies
cargo geiger     # unsafe code in sge and its dependencies
```

## Code of conduct

Taking part in this project means following its
[Code of Conduct](CODE_OF_CONDUCT.md).
