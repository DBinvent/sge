# Rumba[rdbm]
The Relation DB Management tool.

## SQL script tracker and DB deployment tool by schema definition for Postgres

Combines two ways:

1. Declarative for rapid development,
    with schema definition, comparison and versioning
    
2. Imperative for full review and control,
    with script execution tracking by various of filter includes versions and tags


Also, suitable as orchestrator for multiple db instances, hosts and environments.
The app intend to be a microservice ready with multi-database (microservice configuration) & (multi)environment support


#### Key features

 - create database if absent
 - ability to connect by certs
 - use both: schema/table definition and script execution tracking
 - support separate db connection for history table as a remote copy or primary one
 - use psql client for script execution, include native psql script support
 - no JVM nor Python required to run
 - lightweight (just about 10MB)
 - transactional changes with rollback OR use configurable non-transactional mode
 - dry run and diff with existing DB
 - free personal license


#### Limitations

 - Postgres DB only, psql client required
 - see 'Schema deploy restriction'


### Schema/Table deploy by definition restriction notice

 - Schemas and tables definition allowed only in YAML format (JSON file format comming soon) 
 - Adding sql & constraint on existing column won't produce an ALTER TABLE SQL (except FK)
 - The composite primary key (multi columns) will fail, as well as FK to those column. 
 - The columns use by some ORM framework might be sensitive to the column order.
   So, because Postgres does not support inserting column, list column of used in such ORM must append. 
 

### Schema snapshot restriction notice

 - Only column name and type, pk, will be created, but not null and fk constraint 
 - No indexes, constraint, special sql on table or column definition


### TODO

- Add APT repository for continuous upgrade and delivery (cargo-deb)
- Lift column restrictions for Schema snapshot and deploy for multi-primary keys, etc
- Add support of changing and extending datatype: "ALTER TABLE ... SET DATA TYPE"
- Add --schema_transaction: 'retry' to support non-blocking changes on highload 
- Add JSON format for schema definition
- Add schema(table) definition features: view, foreign tables, partitioned tables, indexes, constraint
- Add data snapshot (use pg_dump) full for featuring reverse eng.
- Add data ELT migrations for small dictionary tables 
- Add key-vault's cloud integration for DB connection
- Add UI / api to work as a service (php-admin inspired) and allow scheduled deploy

### License

This software distributed as commercially licensed and free for personal use

