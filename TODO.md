# Schema Guard Enterprise [sge]
The relational database schema management tool.

- Added --schema_transaction: 'retry' to support non-blocking changes on highload 

### TODO

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


