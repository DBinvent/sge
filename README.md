# rdbm
SQL script tracker and DB deployment by schema definition for Postgres

Combined two way:

1.
declarative (liquibase inspired)
    with schema definition
2.
imperative (flyway inspired)
    with script execution tracking

orchestrator for multiple db instances, hosts and environments
microservise ready with multidatabase (microservice configuration) & (multi)environment support
(TODO:) and UI / api to rdbm as a service (phpadmin inspired)

TODO:
scheduled deploy


Schema deploy restriction notice:
The column definition includes 'sql' and constraints (except FK) will apply only for a new columns or new table.
Adding constraint on existing column wont produce an ALTER TABLE SQL  (except FK)
 
The multi primary key column will fail. 
The multi column FK definition column will fail. 

 