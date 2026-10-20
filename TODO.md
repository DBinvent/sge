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


