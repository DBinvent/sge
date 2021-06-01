# rdbm
The Relation DB Management system.

## SQL script tracker and DB deployment by schema definition for Postgres

Combines two ways:

1. Declarative (liquibase inspired)
    with schema definition, comparison and versioning
    
2. Imperative (flyway inspired)
    with script execution tracking by various of filter includes versions and tags


Also, suitable as orchestrator for multiple db instances, hosts and environments.
The app intend to be a microservice ready with multi-database (microservice configuration) & (multi)environment support


#### Key features

 - create database if absent
 - ability to connect by certs
 - use both: schema/table definition and script execution tracking
 - support separate db connection for history table
 - use psql client for script execution, include native psql script support
 - no JVM nor Python required to run
 - lightweight (just about 10MB) and therefore fast
 - transactional changes with rollback OR use configurable non-transactional mode
 - dry run and diff in evaluation license


#### Limitations

 - Postgres DB only
 - see 'Schema deploy restriction'


### Schema/Table deploy restriction notice

 - Schemas and tables definition allowed only in YAML format (JSON comming soon) 
 - Adding sql & constraint on existing column won't produce an ALTER TABLE SQL (except FK)
 - The composite primary key (multi columns) will fail, as well as FK to those column. 
 - The columns use by some ORM framework might be sensitive to column order. So, because Postgres does not support inserting column, list column of used in such ORM must append. 

### Schema snapshot restriction notice

 - only column name and type, pk, not null and fk constraint 
 - no indexes, constraint, special sql on table or column definition


### TODO

- Lift column restrictions for Schema snapshot and deploy for multi-primary keys, etc
- Add changing/extending datatype: "ALTER TABLE ... SET DATA TYPE"
- Add JSON format for schema definition, yaml to json converter, and schema validation https://docs.rs/jsonschema/0.9.0/jsonschema/index.html
- Add schema features: view, foreign tables, partitioned tables, indexes, constraint
- Add data snapshot (pg_dump) full for featuring reverse eng.
- Add data ELT migrations for dictionary tables 
- Add git as a source for scripts and license (pull/push)
- Add key-vault's cloud integration for DB connection and license store
- Add UI / api to work as a service (php-admin inspired)
- Add scheduled deploy

### License

This software distributed as commercially licensed and free for personal use

