# Schema Guard Enterprise [sge]
The relational database schema management tool.

- Added --schema_transaction: 'retry' to support non-blocking changes on highload 

### TODO

- Add SQLite and MySQL as targets beside PostgreSQL (owner, 2026-11-17): the schema definition, its
  diff and deploy, the history table and the scripts on each - their dialects and catalogs
  (MySQL's information_schema). In schema_guard_tokio 2.4 now (2026-11-19), sge's features `sqlite`
  and `mysql` take them in: SQLite's catalog and deploy through turso, the embedded SQLite yt-lite
  uses (src/sqlite.rs), MySQL's through sqlx (src/mysql.rs, tried on a live MySQL 8.0), and the
  mappings each way between the three. sge-sqlite and sge-mysql are built (2026-11-19, src/xdb.rs):
  the history table on each, schemas mapped for the target, scripts by sqlite3 and mysql, --plan,
  --snapshot. Left: sge's other options on them (config file, data files, compare, digest, git and
  zip sources), and the image and rpm builds of them run once on lab10
  - a binary a database (owner, 2026-11-17), each with its feature: the libraries are split by
    them already (schema_guard_tokio, sge and marg, postgres by default)
  - SQL files applied by the database's own client, as psql is for PostgreSQL: sqlite3, mysql,
    sqlplus or SQLcl; the YAML schema applied by schema_guard_tokio through a driver
    (owner, 2026-11-18)
  - Oracle: sge-oracle ships none of Oracle's software (owner, 2026-11-18): Instant Client (the
    `oracle` crate's ODPI-C) and sqlplus or SQLcl installed by the user, checked at the start and
    what is missing said
  - the compatibility guard: each vendor's binary runs schema_guard_tokio's mapping
    (src/mapping.rs, src/mappings/) before its deploy, with a target flag; the mappings each way
    between PostgreSQL, MySQL and SQLite are there (2026-11-19), a matrix test for each
- Partial Done: Lift column restrictions for Schema snapshot and deploy for composite-primary keys, etc
- Add support of changing and extending datatype: "ALTER TABLE ... SET DATA TYPE"
- Add ETL NULL convert IF string type changed to NOT NULL
- Add JSON format for schema definition
- Add partitioning 
- Add data snapshot (use pg_dump) full for featuring reverse eng.
- Add blob / S3 script storage: fetch the object into the temp directory, then run it as
  `source_zip` runs (src/dbm_zip.rs, "Next: blob and S3 storage"); credentials from the
  platform's environment, never argv
- Add `--snapshot_data` table patterns, a mix only: `app.*` (a schema's tables), `orders_*`
  (by name, in any schema), `app.ord*`; a sole `*` or `*.*` refused with an error of its own -
  no dump of every table's data by accident - and so is a pattern with `:view` (one view,
  many tables). Match in `Etl::extract` (snapshot) and `Etl::find` (the `--etl` rule's scope
  on load), drop the unused `is_anytable` (src/dbm_etl.rs); the help says "not supported
  yet" until then - today any `*` ends in SGE-E403
- Add masking of sensitive data in the `--snapshot_data` dump: a rules file mapping
  `schema.table.column` -> a rule - password, email, phone, hash, null, a constant - or a
  script to call; applied as rows are extracted, so no clear value reaches the CSV, and on
  load as well (the `etl` todo in src/i18n/*_clc.yaml). Today: `--snapshot_data=app.users:app.users_masked`,
  a view with the table's column names, masked values in them


