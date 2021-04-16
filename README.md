# rdbm
The Relation DB Management system.

## SQL script tracker and DB deployment by schema definition for Postgres

Combined two ways:

1. Declarative (liquibase inspired)
    with schema definition, comparison and versioning
    
2. Imperative (flyway inspired)
    with script execution tracking by various of filter includes versions and tags


Also suitable as orchestrator for multiple db instances, hosts and environments.
The app intend to be a microservise ready with multidatabase (microservice configuration) & (multi)environment support


## Schema deploy restriction notice:
 - The column definition includes 'sql' and constraints (except FK) will apply only for a new columns or new table.
 - Adding constraint on existing column wont produce an ALTER TABLE SQL  (except FK)
 - The multi primary key column will fail. 
 - The multi column FK definition column will fail. 


## TODO

- Lift column restrictions and 
- Add UI / api to work as a service (phpadmin inspired)
- Also scheduled deploy

