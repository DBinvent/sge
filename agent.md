# agent.md - sge (Schema Guard Enterprise)

A guide for coding agents (and people) who use or change this crate (`schema_guard`, library
`sge`, binary `sge`). Checked against the source for version 2.4.0. What sge does for its
user is in `README.md`; this file is what to know before changing it.

## What it is

sge deploys a database's schema and scripts and keeps their history: YAML schemas through
the `schema_guard_tokio` library (a driver, inside a transaction), SQL scripts through the
database's own client tool (`psql` for PostgreSQL), each run recorded in its history table.
It snapshots a live database back to YAML, diffs, and checks.

## Features: a database each (since 2.4.0)

| feature | what |
|---|---|
| `postgres` (default) | tokio-postgres and `schema_guard_tokio/postgres`: every module that talks to a database |
| `cli` (default) | the command line (clap); the `sge` binary needs `cli` and `postgres` |
| `superset` (default) | `--snapshot_format=superset`, Apache Superset's import bundle |
| `tls` | native-tls connections; implies `postgres` |
| `sqlite` | schema_guard_tokio's SQLite deploy and catalog read (turso, embedded): `sge::sg::sqlite` |
| `mysql` | schema_guard_tokio's MySQL deploy and catalog read (sqlx): `sge::sg::mysql` |

- A library user with `default-features = false` names what it needs, e.g.
  `features = ["postgres"]`; with none, only the constants and `quote_ident` /
  `quote_literal` are left.
- **One binary per database** is the plan: `sge` is PostgreSQL's; an SQLite one, then MySQL
  and Oracle, each with its own feature, its client tool (`sqlite3`, `mysql`, `sqlplus` or
  SQLcl - the user installs Oracle's) and its history table. `schema_guard_tokio` has SQLite's
  and MySQL's catalog and deploy already (features `sqlite`, `mysql`, taken in by sge's of the
  same names).
- **sge-sqlite, sge-mysql** (`src/xdb.rs`, `src/xdb_cli.rs`, `src/bin_sqlite.rs`, `src/bin_mysql.rs`;
  2026-11-19): the core only - a directory in sge's order (S, V, R files), each file once in
  `sge_history` (hl_table.yaml mapped for the target), YAML schemas mapped then deployed by
  schema_guard_tokio, scripts by `sqlite3` / `mysql`, `--plan`, `--snapshot`. Each built with its
  feature only: `cargo build --no-default-features --features cli,sqlite --bin sge-sqlite`. Unit
  tests: `cargo test --lib --no-default-features --features cli,sqlite,mysql xdb` (SQLite in a temp
  file, no sqlite3 needed); a live MySQL run was made by hand on a throwaway container.
- The integration tests (`tests/integration-test*.rs`) carry
  `required-features = ["postgres", "cli"]`.

## Source and cross-vendor schemas

- A snapshot writes `source: postgres.<major>` first (`dbm_log.rs` `load`, from
  `server_version_num`; `postgres.N.M` before 10), so a schema says what wrote it.
- `schema_guard_tokio::mapping` turns a schema of one vendor into another's by declared
  rules (types, defaults, features), the nearest version case applying. Custom SQL of the
  schema is the source vendor's and is ignored on a cross-vendor apply; `sql: UNIQUE` and
  the like become schema fields first. Nothing in sge calls the mapping yet: the target
  flag of the per-database binaries will.

## Working on it

```sh
cargo test --lib --bins          # the unit tests: no database
cargo check --no-default-features
```

- The integration tests need a PostgreSQL server and change it: run them only on one made
  for them.
- **An unreleased schema_guard_tokio:** sge depends on `^2.4.0`; until it is on crates.io,
  build against a local copy with
  `--config 'patch.crates-io.schema_guard_tokio.path="../schema_guard"'` and put
  `Cargo.lock` back afterwards (`git checkout -- Cargo.lock`).
- **Licenses:** `dist/build.sh` writes `THIRD-PARTY-LICENSES.txt` (each library's license,
  copyright and notice texts, sge's and diff_data's) into the deb and the rpm;
  `.github/workflows/sbom.yml` publishes the CycloneDX SBOMs to this repository's `sbom`
  branch.
- A change to the YAML format is `schema_guard_tokio`'s (`src/schema.yaml` there), not sge's.

## State on 2026-11-21

- schema_guard_tokio ^2.5.0, uncommitted (a local `.cargo/config.toml` patch, in `.git/info/exclude`, until 2.5.0
  is published): a table's `security: {schemaPath, alias, softDelete}` (the key `labac` before) deploys through sg as any
  other part of a YAML; a dump of a database writes back the declarations its registry (`labac.tables`) holds,
  an alias left out where it is the entity's own. The struct literals of `Table` and `PgTable` took `security`.
  sge builds its `MigrationOptions` with sg's builder. The mode is the deploy's: `sg_migration` takes
  `security=lsr|rls`, any case; not said, `off`, or a word that is no mode is off - nothing of a table's
  `security` deployed (`security_option`; a word that is no mode is said in the log, `lib.sg_security`).
  sge reads several schema files, so a `security` path may leave the file and its lsr is `Lsr(false)`, without
  sg's one-file checks - deploy the file that defines an entity before the ones that reach it, or say the alias.
  sge steps through a file's tables itself (`migrate_db`: `deploy_all_tables_plan_pg`, or table by table), so it
  runs sg's security steps itself too: `security::link` as a file is parsed (`dbm_files::parse_schema_file`) and
  `security::deploy_all` after the tables, before the foreign keys, in both paths - a table's deploy alone leaves
  its `security` undeployed (review 2026-11-22; test `a_schema_files_security_is_deployed`, a real server).

## State on 2026-11-19

- Version 2.4.0, uncommitted, not published: the database features, the `source` line of a
  snapshot (its fixtures say `source: postgres.$VERSION`), `bpchar(n)` in snapshots. 2.4, not
  3: `postgres` is a default (owner, 2026-11-19).
- Published after schema_guard_tokio 2.4.0, which it needs.
