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
 - lightweight (just 8MB) and therefore fast
 - transactional changes with rollback and configurable non-transactional mode
 - dry run and diff in evaluation license


#### Limitations

 - Postgres DB only
 - see 'Schema deploy restriction'


### Schema/Table deploy restriction notice

 - Schemas and tables definition allowed only in YAML format 
 - The column definitions 'sql' field and constraints (except FK) will apply only for a new columns or new table
 - Adding constraint on existing column won't produce an ALTER TABLE SQL (except FK)
 - The multi primary key columns will fail. 
 - The multi column FK definition column will fail. 
 - All script files executed by psql with a flag --single-transaction, to be able to correctly handle a status and safely repeat


### Schema snapshot restriction notice

 - only column name and type
 - no indexes, foreign keys, constraint, special sql on table or column definition


### TODO

- Lift column restrictions for Schema snapshot and deploy for multi-primary keys, etc
- Add JSON format for schema definition
- Add git as a source for scripts and license
- Add key-vault's cloud integration for DB connection and license store
- Add UI / api to work as a service (php-admin inspired)
- Add scheduled deploy
- Add data migration: dump - upgrade 

### License

This software distributed as commercially licensed

