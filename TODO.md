# Rumba[rdbm]
The Relation DB Management tool.

### TODO

- Lift column restrictions for Schema snapshot and deploy for composite-primary keys, etc
- Add support of changing and extending datatype: "ALTER TABLE ... SET DATA TYPE"
- Add --schema_transaction: 'retry' to support non-blocking changes on highload 
- Add roles granting
- Add JSON format for schema definition
- Add schema(table) definition features: view, foreign tables, partitioned tables, indexes, constraint, sp/f
- Add partitioning 
- Add data snapshot (use pg_dump) full for featuring reverse eng.
- Add data ELT migrations for small dictionary tables
  - Dump to parquet?
  - generate SQL: https://www.postgresql.org/docs/current/sql-insert.html with update on conflict 
- Add key-vault's cloud integration for DB connection
- Add UI / api to work as a service (php-admin inspired) and allow scheduled deploy

### License

This software distributed as commercially licensed and free for personal use

