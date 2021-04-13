# rdbm
SQL deployment tracker and schema def on Rust for Postgres

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