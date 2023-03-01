# Rumba:\[rdbm\]
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
 - simple ETL for extraction on snapshot and load data on migration
 - support separate db connection for history table as a remote copy or primary one
 - use psql client for script execution, include native psql script support
 - no JVM nor Python required to run
 - lightweight (just about 10MB)
 - transactional changes with rollback OR use configurable non-transactional mode
 - dry run and diff with existing DB
 - free personal license


#### Limitations

 - Postgres DB only, psql client required
 - see 'Schema deploy restriction notice' bellow


### Schema/Table deploy by definition restriction notice

 - Schemas and tables definition allowed only in YAML format (JSON file format coming soon) 
 - Adding sql & constraint on existing column/tables won't produce an ALTER TABLE SQL (except FK but sql)
 - The composite primary key (multi columns) only on create table 
 

### Schema snapshot restriction notice

 - Only column name and type, pk, will be created, but not null and fk constraint 
 - No indexes, constraint, special sql on table or column definition

### APT repo with DEB package
 
```
wget -O - https://www.dbinvent.com/dist/DBinventLLC.gpg.key | sudo apt-key add -
sudo sh -c 'echo "deb http://apt.dbinvent.com/$(lsb_release -cs) $(lsb_release -cs) non-free" > /etc/apt/sources.list.d/dbinvent.list
sudo apt update && sudo apt install rdbm
```

### RPM
```
sudo rpm --import https://www.dbinvent.com/dist/DBinventLLC.gpg.key
sudo rpm -i https://www.dbinvent.com/dist/rdbm-latest.x86_64.rpm
```
 

### License

This software distributed as commercially licensed and free for personal use

