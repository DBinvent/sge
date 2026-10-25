Integration tests

- Each starts a throwaway PostgreSQL with `./postgres.sh` (in this folder): the
  `initdb` of the version `PG_VER` names (default: the newest installed), found
  under `/usr`, on a
  port of its own, its data under `/tmp/tests`. The PostgreSQL server binaries
  must be installed, and the port free to bind.
- One test function per file, so they run one after another, not in parallel.
- `SGE_IT=ignore` skips them: `SGE_IT=ignore cargo test`. The unit tests alone:
  `cargo test --lib --bins`.

Coverage

- `cargo llvm-cov --no-fail-fast --summary-only`: the whole `cargo test`, unit and
  integration tests together, over `src/`. 93% of the lines - the README's badge,
  set by hand: update it when the figure moves.
