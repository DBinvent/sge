# rdbm
The Relation DB Management system.

## SQL script tracker and DB deployment by schema definition for Postgres

Combines two ways:

1. Declarative (liquibase inspired)
    with schema definition, comparison and versioning
    
2. Imperative (flyway inspired)
    with script execution tracking by various of filter includes versions and tags


Also suitable as orchestrator for multiple db instances, hosts and environments.
The app intend to be a microservise ready with multidatabase (microservice configuration) & (multi)environment support


#### Key features

 - create database if absent
 - connect by certs
 - use both: schema definition and script execution tracking
 - support separate db connection for history table
 - use psql client for script execution
 - no jvm nor python required to run
 - lightweight (just 8MB) and fast


#### Limitations

 - Postgres DB only
 - see 'Schema deploy restriction'


### Schema deploy restriction notice

 - The column definitions 'sql' field and constraints (except FK) will apply only for a new columns or new table.
 - Adding constraint on existing column wont produce an ALTER TABLE SQL (except FK)
 - The multi primary key columns will fail. 
 - The multi column FK definition column will fail. 
 - All script files executed by psql with a flag --single-transaction, to be able to correctly handle a status and safely repeat


### TODO

- Lift column restrictions for Schema deploy for multiprimary keys, etc
- Order column on table creation by yaml appearance
- Add git as a source 
- Add keyvault cloud integration
- Add UI / api to work as a service (phpadmin inspired)
- Add scheduled deploy
- Add data migration: dump - upgrade 

### License

This software is licensing just for $10

