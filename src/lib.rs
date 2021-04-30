#[macro_use]
extern crate slog;
use lazy_static::lazy_static;

extern crate semver;

mod dbm_dbc;
mod dbm_log;
pub mod dbm_yml;
pub mod license;

use crate::dbm_dbc::InfoSchemaType;
use crate::dbm_log::*;
use crate::dbm_yml::*;
use openssl::ssl::{SslConnector, SslMethod};
use postgres::{Client, NoTls, Transaction};
use postgres_openssl::MakeTlsConnector;
use slog::Logger;
use std::fs;

extern crate yaml_rust;

use yaml_rust::YamlLoader;

use chrono::{TimeZone, Utc};
use openssl::error::ErrorStack;
use postgres::error::SqlState;
use std::fmt::Write;
use std::time::SystemTime;
use std::collections::HashMap;

pub const CONFIG_DEF_YML: &str = "
name: rdbm
version: \"1.0\"
author: DBInvent LLC
about: 'The SQL script tracker and DB deployment by YAML schema definition for PostgreSQL. Please use: ./rdbm help migrate, for more information.'
usage: 'Please start with ./rdbm help license and ./rdbm help migrate.'

args:
    - db_name:
        long: db_name
        default_value: postgres
        help: Postgres database
    - db_host:
        long: db_host
        default_value: /var/run/postgresql
        help: Postgres host
    - db_port:
        long: db_port
        default_value: \"5432\"
        help: Postgres port
    - db_user:
        long: db_user
        default_value: postgres
        help: user
    - db_password:
        long: db_password
        takes_value: true
        help: Password or SSL certificates (see db_sslxxx params)
    - db_sslmode:
        long: db_sslmode
        default_value: verify-full
        help: sslmode
    - db_sslrootcert:
        long: db_sslrootcert
        takes_value: true
        help: /path/to/server-ca.crt
    - db_sslcert:
        long: db_sslcert
        takes_value: true
        help: /path/to/client.crt
    - db_sslkey:
        long: db_sslkey
        takes_value: true
        help: /path/to/client.key
    - logdb_name:
        long: logdb_name
        takes_value: true
        help: Postgres database, use 'db_***' configuration if 'logdb_name' not set
    - logdb_host:
        long: logdb_host
        default_value: /var/run/postgresql
        help: Postgres host
    - logdb_port:
        long: logdb_port
        default_value: \"5432\"
        help: Postgres port
    - logdb_user:
        long: logdb_user
        default_value: postgres
        help: user
    - logdb_password:
        long: logdb_password
        takes_value: true
        help: Password or SSL certificates (see xx_sslxxx params)
    - logdb_sslmode:
        long: logdb_sslmode
        default_value: verify-full
        takes_value: true
        help: sslmode
    - logdb_sslrootcert:
        long: logdb_sslrootcert
        takes_value: true
        help: /path/to/server-ca.crt
    - logdb_sslcert:
        long: logdb_sslcert
        takes_value: true
        help: /path/to/client.crt
    - logdb_sslkey:
        long: logdb_sslkey
        takes_value: true
        help: /path/to/client.key
    - pre_deploy:
        long: pre_deploy
        default_value: \"*\"
        help: Scan for files to perform (P)re Deploy scripts. Found file with suitable versions will check for absent in execution and run ahead of (S)chema file.
    - schema_deploy:
        long: schema_deploy
        default_value: \"*\"
        help: Scan for files to perform deploy (S)chema scripts, i.e. target schema files to deploy, up to the target version (* - means all). \
            Yaml schema files will be parsed, compared with DB information schema and tables will append with missed columns. \
            Existing columns wont change neither delete.
    - checksum:
        long: checksum
        default_value: \"true\"
        help: Fail on deploy if scripts checksum mismatch for (P)re and (V)ersion between files already deployed and logged and the the script found in source folder. This helps to keep file to be consistent between environments.
        possible_values: [yes, no, ok, y, \"1\", \"true\", \"false\"]
    - rr_deploy:
        long: rr_deploy
        default_value: \"*\"
        help: Scan for files to perform deploy (R)e-runable scripts. Found file with suitable versions will check for hashsum and if file absent or changes detected, than script will execute.
    - schema:
        long: schema
        default_value: public
        help: The deployment history 'table' schema location
    - schema_transaction:
        long: schema_transaction
        default_value: file
        help: \"The value to override (S)chema file database.schema.table.transaction option \\n \
        - file: (default) execute the (S)chema file in a single transaction. This is guarantee to have all or neither changes applied by the file marked as OK. \\n \
        - table: perform all changes for each table per transaction.  \\n \
        The value pass in parameter will override a value set in (S)chema file on a migration execution.
        \"
    - version_transaction:
        long: version_transaction
        takes_value: true
        help: \"The value to override (V)ersion file execution option in psql \\n \
        - single: (default) execute the (V)ersion file with a --single-transaction psql option. \\n \
        - statement: execute psql without --single-transaction option. This is also available by set first line in file to 'false' like: --single-transaction=false\
        \"
    - table:
        long: table
        default_value: rdbm_history
        help: The deployment history table name in 'schema'
    - source_dir:
        long: source_dir
        default_value: \"./\"
        help: Directory scripts location, use <source_recur> to scan sub dir
    - source_recur:
        long: source_recur
        default_value: \"true\"
        help: Scan Directory recursively
        possible_values: [yes, no, ok, y, \"1\", \"true\", \"false\"]
    - executed_by:
        long: executed_by
        takes_value: true
        help: The user name run the migration
    - script_tag:
        long: script_tag
        takes_value: true
        help: Set the Tag to applied scripts (rows)
    - db_target:
        long: db_target
        takes_value: true
        help: The target is a database name, it's an empty string (if the log history table on same database); OR environment(dev|prod)+host(node)+database for orchestration configuration.
    - db_catalog:
        long: db_catalog
        takes_value: true
        help: The name of set of DB schemas / module / microservice.
    - version_deploy:
        long: version_deploy
        default_value: \"*\"
        help: Perform Deploy (V)ersion files up to the target version (* means all)
    - dry_run:
        long: dry_run
        default_value: \"false\"
        help: \"Generate all SQL, but do not run on DB (EXCEPT 'CREATE DATABASE' / 'CREATE ROLE'), also log all attempts to script file run. To avoid 'CREATE DATABASE'/'CREATE ROLE' SQL execution please remove '--createdb' parameter or set it to false.\"
        possible_values: [yes, no, ok, y, \"1\", \"true\", \"false\"]
    - dry_run_to:
        long: dry_run_to
        takes_value: true
        help: The name of file to dump the generated SQL script
    - snapshot_format:
        long: snapshot_format
        default_value: yaml
        possible_values: [yaml, json]
        help: \"The format of generated information of DB schema by the 'snapshot' command, also see '--snapshot_to' parameter.\"
    - info_format:
        long: info_format
        default_value: json
        possible_values: [yaml, json]
        help: \"The format of generated information of DB schema by the 'info' command\"
    - snapshot_to:
        long: snapshot_to
        takes_value: true
        help: 'The name of file to dump the database structure. Used by snapshot command. default_value is: D<next>.0__<database_name>.yaml in the source_dir.'
    - config_file:
        long: config
        short: c
        takes_value: true
        help: \"Load all params from config file (: or = separated), any command line params will override parameters loaded from file. If neither set than environment variables (with CAPITALIZED names) will be search to use.\"
    - after:
        long: after
        default_value: \"after.sql\"
        takes_value: true
        help: Try to find and run the script after successful migration execution. Do not fail if not found. This script execution wont log into history table.
    - after_each:
        long: after_each
        default_value: \"after_each.sql\"
        takes_value: true
        help: Try to find and run the script after each 'V' or 'R' successful execution. Do not fail if not found. This script execution wont log into history table.
    - createdb:
        long: createdb
        default_value: \"false\"
        help: Create DB if the db_name/log_dbname not exists. Will try to use same credentials and db postgres
        possible_values: [yes, no, ok, y, \"1\", \"true\", \"false\"]
    - verbose:
        long: verbose
        default_value: \"false\"
        help: Turn DEBUG logging ON.
        possible_values: [yes, no, ok, y, \"1\", \"true\", \"false\"]
    - createdb_user:
        long: createdb_user
        default_value: postgres
        help: The DB user to use during 'Create DB' step.
    - createdb_pwd:
        long: createdb_pwd
        takes_value: true
        help: The DB user's password to use during 'Create DB' step. If not set, then will be use db_sslxxx.
    - createdblog_user:
        long: createdblog_user
        default_value: postgres
        help: The DB user to use during 'Create LOGDB' step. LOGDB use if logdb_name set.
    - createlogdb_pwd:
        long: createlogdb_pwd
        takes_value: true
        help: DB user's password to use during 'Create LOGDB' step. If not set, then will be use logdb_sslxxx.
    - email:
        long: email
        takes_value: true
        help: The person's email to send a license file.
    - subscribe:
        long: subscribe
        takes_value: true
        help: 'Use to append license with connections: <db_name> or <host>. possible values: * OR <name>.'
    - license_file:
        long: license_file
        default_value: rdbm.license
        help: The license file. Use './rdbm --email=your@email license' OR check 'http://rdbm.dbinvent.com/' to get one. Also use the source_dir to search.


subcommands:
    - migrate:
        about: Perform a migration to specified version of all possible scripts
        help: \" Perform a DB migration to a version set in '--pre_deploy', '--schema_deploy', '--version_deploy', '--rr_deploy' of scripts located in '--source' \
        (without a sub-folder scan if '--source_recur' is disabled) \
    into the DB name set in '--db_name', with execution log saved into '--logdb_name' or '--db_name' if '--logdb' not set. \\n \
    The DB and DB user will be created if absent, and if '--createdb' set, and '--createdb_xxx' user credential properly configured, regardless of '--dry_run'. \\n \
    The migration executor will be logged by '--executed_by' value \\n \
    Also '--script_tag', '--db_target' and '--db_catalog' will set if present in params or configs. Those values are useful to microservice architecture (see rdbm.dbinvent.com/best for more information).\\n \
    \\n The execution order is: (P)->(S)->(V)->(R)->(after) \\n \\n \
    i.e. (P)reDeploy SQL, (S)chema YAML, (V)ersion SQL, (R)e-runable SQL, (a)fter (each) migration SQL.\\n \
    The (P)reDeploy SQL use to be executed before (S)chema YAML.\\n \
    The (S)chema YAML will try to scan an information DB and apply changes required changes.\\n \
    The (V)ersion SQL intended to be a major set of files for DB upgrade and will be executed only one time and store into a history log table.\\n \
    The (R)e-runnable SQL very useful for handling Store Procedure upgrade and with a '--script_tag' very easily trackable.\\n \
    The (P) and (V) will check for version and execute if version absent in target DB up to a target version. \\n \
    Also (P) and (V) scripts will check for consistency and fail if the script changed and '--checksum' set. \\n \
    The (S) script will check for version target and try to apply changes if they are absent in target DB. \\n \
    The (R) script will check a file hashsum and run it if the file was changed to was not run yet on DB. \
    The 'after.sql' and 'after_each.sql' (names are configurable) will try always execute, but wont record in the history. \\n \
    The '--config_file' or '-c' use to put all params by name (without -- prefix) into a text file. The args pass in command line will override those from config file. \\n \
    See more options in './rdbm help' command.\\n \
    Use --dry_run to avoid DB change. \\n Require a valid license to run (use './rdbm help license' for more information). \\n \
    Check https://rdbm.dbinvent.com for more information and best practice.\"
    - prepare:
        about: Create a history log table use to track scripts executions
        help: \"Create a history log table defined in '--table' placed in '--schema' in the '--logdb_name' database, if set, or '--db_name' if not set. Use '--dry_run' to avoid DB change.\"
    - snapshot:
        about: Perform a schema dump into snapshot_to file in yaml or json format.
        help: \"Perform a DB schema dump into a '--snapshot_to' file in yaml or json format. Only column name and type information for now will be stored.\"
    - info:
        about: Return deployment status and a tag version in JSON format. See '--info_format'.
        help: Return deployment status and a tag version in JSON format. See '--info_format'.
    - license:
        about: Create apply, upgrade, subscribe or check a license to use this software.
        help: \"Create apply, upgrade, subscribe or check a license.\n\
        Use '--email' and '--license' to request a license file if the file not found in '--license_file' OR Evaluation is expired.\\n \
        If the file exist, than will try to check and upgrade a licenses 'valid_until' date.\\n \
        Use '--subscribe=*' to add a connection name (DB name & host to the list of licensed connections). \\n \
        This is the only the command of ./rdbm which making a connection to the https://rdbm.dbinvent.com and require a valid 'api_key' to perform upgrade or subscribe. \\n \
        Check https://rdbm.dbinvent.com/license for more information about licensing.\"
";


// --schema_transaction:
//     - (column-TBD): perform all changes on a table per transaction. \\n \
//     - (repeat-TBD): try to obtain a lock perform changes repetally - suitable on high load systems \\n \

pub(crate) const BASELINE: &str = "baseline";

const STATEMENT_LOG_TABLE_YML: &str = "
tableName: rdbm_history
description: The schema deployment log
columns:
    - column:
        name: id
        type: serial
        constraint:
            primaryKey: true
            nullable: false
    - column:
        name: source
        type: varchar(250)
        constraint:
            nullable: false
    - column:
        name: target
        type: varchar(250)
    - column:
        name: source_type
        type: char
        description: \"S: Schema, V: Version, R: Re-runnable\"
        constraint:
            nullable: false
    - column:
        name: version
        type: varchar(80)
        constraint:
            nullable: false
    - column:
        name: created_on
        type: timestamp
        defaultValue: current_timestamp
        constraint:
            nullable: false
    - column:
        name: checksum
        type: varchar(80)
    - column:
        name: scheduled_on
        type: timestamp
    - column:
        name: script_tag
        type: varchar(80)
    - column:
        name: catalog
        type: varchar(80)
    - column:
        name: executed_on
        type: timestamp WITH TIME ZONE
    - column:
        name: executed_by
        type: varchar(80)
    - column:
        name: ok
        type: bool
        constraint:
          nullable: false
        defaultValue: \"false\"
        description: Successful execution of the script
    - column:
        name: msg
        type: varchar(250)
    - column:
        name: took_sec
        type: bigint
        constraint:
            nullable: false
        defaultValue: \"0\"
    - column:
        name: times
        type: int
        defaultValue: '1'
        constraint:
            nullable: false
";

/// Max varchar len for columns name: source, target, msg
pub(crate) const SRC_T_M_LEN: usize = 250;
/// Max varchar len for all other columns names
pub(crate) const SLT_C_LEN: usize = 80;

lazy_static! {
    pub static ref STATEMENT_LOG_TBL: crate::dbm_yml::YTable =
        match YamlLoader::load_from_str(STATEMENT_LOG_TABLE_YML) {
            Ok(y) => {
                if y.len() > 0 {
                    let y = &y[0];
                    YTable::new(&y, y["tableName"].as_str().unwrap(), None, None).unwrap()
                } else {
                    panic!("zero config");
                }
            }
            Err(e) => {
                panic!("wrong config: {}", e);
            }
        };

    pub static ref CONFIG_APP: HashMap<String, Option<String>> = match YamlLoader::load_from_str(crate::CONFIG_DEF_YML) {
        Ok(yaml) => {
                let mut defs = HashMap::new();
                for args in yaml[0]["args"].as_vec().unwrap() {
                    for (k, v) in args.as_hash().unwrap() {
                        if let Some(k) = k.as_str() {
                            defs.insert(k.into(), v["default_value"].as_str().map(|v| v.into()));
                        }
                    }
                }
                defs }
        Err(e) => {
            panic!("wrong config: {}", e);
        }
    };
    static ref EPOC_2020: i64 = Utc.ymd(2020, 2, 2).and_hms(0, 0, 0).timestamp();
    static ref EPOC_2020H: u64 = EPOC_2020.clone() as u64 >> 12;
}

/// pseudo hours (real hours + 14% as divided to 4096 (>> 12) instead of 3600)
/// science 2020
#[inline]
pub(crate) fn _hp() -> u32 {
    let r = if let Ok(n) = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        (n.as_secs() >> 12) - *EPOC_2020H
    } else {
        EPOC_2020.clone() as u64 >> 12
    };
    r as u32
}

/**
##Description:
Relation (PostgreSQL) DB Migration Management tool (inspired by flyway while it was free)

Library Key Features:

1. Check schema - change log
    S
2. Migrate (upgrade)
    V
2.1. Downgrade (optional)
3. Re-runnable with tag tracking
    R
4. Test: migrate, downgrade, rerun
5. Dry run (optional)
6. psql environment for script run


runs by tests
*/
pub fn migrate(cfg: ConfigMap, prepare_only: bool, log: &Logger) -> Result<(), String> {
    let mut client = connect("logdb_", &cfg, None, log)?;
    let result = { migrate_db(&mut client, &cfg, prepare_only, log) };
    match client.close() {
        Ok(_) => {}
        Err(_) => {}
    }
    result
}

/// Stesp on execute:
/// * connect to DB
/// * self create history table
/// * load all matched schemas from pg_catalog
/// * load all tables columns and triggers from information_schema by schemas
/// * load all file names from dir cfg
/// - lock history table to prevent parallel dbm execution
/// - run pre_run files, if exists
/// - migrate 'S'chema files, save log
/// - migrate 'V'ersion files, save log
/// - define re-run by logs
/// - migrate 'R'e-runable files, save log
/// - run post_run files
#[inline]
fn migrate_db(
    log_client: &mut Client,
    cfg: &ConfigMap,
    prepare_only: bool,
    log: &Logger,
) -> Result<(), String> {
    // prepare schema
    let schema = cfg.schema();
    let mut dbc = crate::dbm_dbc::load_info_schema(log_client)?;
    let yschemas = crate::dbm_yml::load_all(cfg, log)?;
    let mut t = log_client.transaction().unwrap(); // begin self install if not exists
    let err = {
        debug!(log, "starting with self deploy for: {}", BASELINE);
        match STATEMENT_LOG_TBL.deploy(&mut dbc, &mut t, &schema, cfg, BASELINE, log) {
            Ok(exec) => {
                if exec {
                    debug!(log, "self deploy of: {} complete", BASELINE);
                    if let Err(e) = StatementLog::new(cfg)
                        .instance('S', &BASELINE.into(), &BASELINE.into())
                        .persist(None, &mut t)
                    {
                        Some(e)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            Err(e) => Some(e),
        }
    };
    if let Some(e) = err {
        warn!(log, "rollback on fail self deploy: {}", e);
        if let Err(e) = t.rollback() {
            warn!(log, "rollback on fail self deploy failed: {}", e);
        }
        return Err(e);
    };
    let res = if prepare_only {
        Ok(())
    } else {
        // select table for update -- row level lock, revert changes
        let logs = { StatementLog::load_all(&mut t, cfg.get_bool("dry_run", true), cfg)? };
        let pgsrv = format!("/tmp/.rdbm_{}.conf", cfg.get("db_name"));
        let res = match deploy_pre_files(&logs, &yschemas, pgsrv.as_str(), &cfg, &mut t, log) {
            Ok(_) => match deploy_all_schemas(&yschemas, &mut dbc, &cfg, &mut t, log) {
                Ok(_) => deploy_all_files(&logs, &yschemas, pgsrv.as_str(), &cfg, &mut t, log),
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        if cfg.is_pgsrv() {
            let _ = fs::remove_file(pgsrv);
        }
        res
    };
    match t.commit() {
        Ok(()) => res,
        Err(e) => {
            info!(log, "commit history failed: {}", e);
            match res {
                Ok(_) => Err(format!("commit history failed: {}", e)),
                Err(ee) => Err(format!("{}. Also, commit history failed: {}", e, ee)),
            }
        }
    }
}

#[inline]
/// deploy changes by files
fn deploy_pre_files(
    logs: &LogSchemaType,
    yschemas: &FilesMap,
    pgsrv: &str,
    cfg: &ConfigMap,
    t: &mut Transaction,
    log: &Logger,
) -> Result<(), String> {
    if cfg.is_pgsrv() {
        let mut content = String::new();
        let names = vec![
            "host",
            "port",
            "user",
            "password",
            "sslmode",
            "sslrootcert",
            "sslcert",
            "sslkey",
        ];
        for name in names.iter() {
            cfg.get_pow(&mut content, "db_", name, "\n");
        }
        if let Err(e) = fs::write(pgsrv, format!("[{}]\n{}", cfg.get("db_name"), content)) {
            return Err(format!("creating PGSERVICEFILE: {}: {}", pgsrv, e));
        }
    }
    let draft = StatementLog::new(cfg);
    // deploy versions
    for (v, file) in &yschemas.pre {
        draft
            .instance('P', &v.s, file)
            .deploy(&logs, pgsrv, cfg, t, log)?;
        draft.after("after_each", yschemas, &logs, pgsrv, cfg, t, log)?;
    }
    Ok(())
}

#[inline]
/// deploy changes by files
fn deploy_all_files(
    logs: &LogSchemaType,
    yschemas: &FilesMap,
    pgsrv: &str,
    cfg: &ConfigMap,
    t: &mut Transaction,
    log: &Logger,
) -> Result<(), String> {
    let draft = StatementLog::new(cfg);
    // deploy versions
    for (v, file) in &yschemas.versions {
        draft
            .instance('V', &v.s, file)
            .deploy(&logs, pgsrv, cfg, t, log)?;
        draft.after("after_each", yschemas, &logs, pgsrv, cfg, t, log)?;
    }
    // deploy re-runable
    for (v, file) in &yschemas.re_runnable {
        draft
            .instance('R', &v.s, file)
            .deploy(&logs, pgsrv, cfg, t, log)?;
        draft.after("after_each", yschemas, &logs, pgsrv, cfg, t, log)?;
    }
    draft.after("after", yschemas, &logs, pgsrv, cfg, t, log)?;
    Ok(())
}

#[inline]
/// deploy schema by definitions - declaratives
fn deploy_all_schemas(
    yschemas: &FilesMap,
    dbc: &mut InfoSchemaType,
    cfg: &ConfigMap,
    lt: &mut Transaction,
    log: &Logger,
) -> Result<(), String> {
    let mut err = None;
    let mut client = connect("db_", &cfg, None, log)?;
    {
        let stl = StatementLog::new(cfg);
        let table_tr = cfg.get("schema_transaction") == "table";
        let mut do_persist = false;
        for (ver, (file, ysv)) in &yschemas.schemas {
            match client.transaction() {
                Ok(mut t) => {
                    // let mut t = client.transaction().unwrap();
                    for (s, ys) in ysv {
                        for  yt in &ys.tables.list {
                            match yt.deploy(dbc, &mut t, &s, &cfg, &ys.file, log) {
                                Ok(ok) => {
                                    if ok {
                                        do_persist = true;
                                        if table_tr {
                                            if let Err(ec) = t.batch_execute("COMMIT; BEGIN") {
                                                do_persist = true;
                                                err = Some(format!("committing change on {}.{} with {}", s, yt.table_name, ec));
                                                break;
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    do_persist = true;
                                    err = Some(e);
                                    break;
                                }
                            }
                        }
                    }

                    if let None = &err {
                        // deploy FK
                        // for (ver, (file, ysv)) in &yschemas.schemas {
                        for (s, ys) in ysv {
                            for yt in &ys.tables.list {
                                match yt.deploy_fk(&yschemas, dbc, &mut t, &s, &cfg, &ys.file, log) {
                                    Ok(ok) => {
                                        if ok {
                                            do_persist = true;
                                            if table_tr {
                                                if let Err(ec) = t.batch_execute("COMMIT; BEGIN") {
                                                    do_persist = true;
                                                    err = Some(format!("committing change on {}.{} with {}", s, yt.table_name, ec));
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        err = Some(e);
                                        break;
                                    }
                                }
                            }
                            if let Some(_) = &err {
                                break;
                            }
                        }
                    }

                    if table_tr || ! do_persist {
                        let _ = t.rollback(); // done with transaction anyway
                    } else {
                        match &err {
                            // err means was persist
                            Some(_) => {
                                let _ = t.rollback();
                            }
                            None => {
                                if do_persist {
                                    if let Err(e) = t.commit() {
                                        err = Some(format!("committing change for {} {}", file, e))
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    err = Some(format!("creating transaction for {} {}", file, e))
                }
            }
            if do_persist {
                if let Err(e) = stl.instance('S', &ver.s, file).persist(err.clone(), lt) {
                    err = Some(e);
                }
            }
            if let Some(_) = &err {
                break;
            }
        }

    }
    let _ = client.close();
    match err {
        None => Ok(()),
        Some(e) => Err(e),
    }
}

#[inline]
fn connect(
    prefix: &str,
    cfg: &ConfigMap,
    create: Option<bool>,
    log: &Logger,
) -> Result<Client, String> {
    let mut override_prefix = false;
    if prefix == "logdb_" {
        if let None = cfg.get_opt("logdb_name") {
            override_prefix = true;
        }
    };
    let prefix = if override_prefix { "db_" } else { prefix };
    let mut url = String::new();
    // todo test with cert, make condition packaging
    let names = vec![
        "host",
        "port", // "user", "password",
        "sslmode",
        "sslrootcert",
        "sslcert",
        "sslkey",
    ];
    for name in names.iter() {
        cfg.get_pow(&mut url, prefix, name, "&");
    }

    let as_postgres_user = match create {
        None => false,
        Some(pc) => pc,
    };

    let (dbname, log_url) = if as_postgres_user {
        let _ = write!(
            url,
            "{}user={}",
            if url.len() > 0 { "&" } else { "" },
            cfg.get_def(format!("create{}user", prefix).as_str(),
                cfg.get(format!("{}user", prefix).as_str()))
        );

        let mut log_url = url.clone();
        match cfg.get_opt(format!("create{}pwd", prefix).as_str()) {
            None => {
                cfg.get_pow(&mut url, prefix, "password", "&");
                cfg.get_pow_scr(&mut log_url, prefix, "password", "&");
            }
            Some(pwd) => {
                let _ = write!(url, "&password={}", pwd);
                let _ = write!(log_url, "&password={}", screen(&pwd));
            }
        }
        debug!(log, "create db connect: {}", log_url);
        ("postgres".to_string(), log_url)
    } else {
        cfg.get_pow(&mut url, prefix, "user", "&");
        let mut log_url = url.clone();
        cfg.get_pow(&mut url, prefix, "password", "&");
        cfg.get_pow_scr(&mut log_url, prefix, "password", "&");
        match cfg.get_opt(format!("{}name", prefix).as_str()) {
            None => {
                return Err(format!("No DB name set: {}name", prefix));
            }
            Some(db) => (db, log_url),
        }
    };
    let db = format!("postgresql:///{}?{}", dbname, url);
    debug!(log, "connecting: {}", log_url);
    match if cfg.is_ssl() {
        let connector = match make_tls_connector(&cfg.get(prefix)) {
            Ok(c) => c,
            Err(e) => {
                return Err(format!("create TLS(SSL) connection: {}", e));
            }
        };
        Client::connect(db.as_str(), connector)
    } else {
        Client::connect(db.as_str(), NoTls)
    } {
        Ok(client) => Ok(client),
        Err(err) => {
            if let None = create {
                if let Some(code) = err.code() {
                    if code == &SqlState::UNDEFINED_DATABASE
                        || code == &SqlState::INVALID_PASSWORD
                        || &code.code()[..5] == "0800"
                    {
                        if cfg.get_bool("createdb", false) {
                            return create_connect(code == &SqlState::UNDEFINED_DATABASE, dbname.as_str(), prefix, cfg, log);
                        } else {
                            error!(log, "NO DB: {}, create or use --createdb=y", dbname);
                        }
                    } else {
                        debug!(log, "connection error code: {:?}", code);
                    }
                }
            }
            Err(format!(
                "Connect to Postgres [{}] {}: {} ",
                log_url, dbname, err
            ))
        }
    }
}

#[cfg(debug_assertions)]
#[inline]
fn screen(pwd: &String) -> &str {
    pwd.as_str()
}

#[cfg(not(debug_assertions))]
#[inline]
fn screen(_pwd: &String) -> &str {
    "***"
}


#[inline(never)]
fn create_connect(
    create_db: bool,
    dbname: &str,
    prefix: &str,
    cfg: &ConfigMap,
    log: &Logger,
) -> Result<Client, String> {
    info!(log, "NO DB name [{}] or user found, trying to connect as super user ...", dbname);
    let mut client = connect(prefix, cfg, Some(true), log)?;
    let owner = match cfg.get_opt(format!("{}user", prefix).as_str()) {
        None => "".to_string(),
        Some(user) => {
            debug!(log, "testing for user: {}", user);
            create_role(&mut client, &user,  prefix, cfg, log)?;
            format!(" OWNER {}", user)
        },
    };
    if create_db {
        info!(log, "NO DB name [{}] found, trying to create...", dbname);
        let _ = client.batch_execute(format!("CREATE DATABASE {} {}", dbname, owner).as_str())
            .map_err(|e| format!("Creating DB [{}] by postgres: {} ", dbname, e))?;
    }
    let _ = client.close();
    info!(log, "DB/user [{}] created, reconnecting", dbname);
    connect(prefix, cfg, Some(false), log)
}

fn create_role(
    db: &mut Client,
    user: &String,
    prefix: &str,
    cfg: &ConfigMap,
    log: &Logger,
) -> Result<(), String> {
    let row = db
        .query_one("SELECT count(rolname) FROM pg_roles WHERE rolname = $1", &[user])
        .map_err(|e| format!("loading role [{}] {}", user, e))?;
    let users: i64 = row.get(0);
    if users == 0 {
        info!(log, "NO user {} found, trying to create ...", user);
        let pwd = match cfg.get_opt(format!("{}password", prefix).as_str()) {
            None => "".to_string(),
            Some(pwd) => format!(" PASSWORD '{}'", pwd)
        };
        let _ = db.batch_execute(format!("CREATE ROLE {} WITH LOGIN {}", user, pwd).as_str())
            .map_err(|e| format!("Creating user role: {} error: {}", user, e))?;
        info!(log, "Created user role: {}", user);
    }
    Ok(())
}

#[inline]
fn make_tls_connector(cert: &String) -> Result<MakeTlsConnector, ErrorStack> {
    let mut builder = SslConnector::builder(SslMethod::tls())?;
    builder.set_ca_file(cert)?;
    Ok(MakeTlsConnector::new(builder.build()))
}

#[inline]
pub fn load(dump: bool, cfg: ConfigMap, log: &Logger) -> Result<(), String> {
    let snapshot_to = cfg.get_opt("snapshot_to");
    let mut log_client = connect("logdb_", &cfg, None, log)?;
    let mut db_client = connect("db_", &cfg, None, log)?;
    let result = { load_impl(dump, &mut db_client, &mut log_client, &cfg) };
    let _ = db_client.close();
    let _ = log_client.close();
    match result {
        Ok(loaded) => {
            if dump {
                match snapshot_to {
                    None => {
                        println!("{}", loaded);
                    }
                    Some(file) => {
                        let _ = fs::write(file, loaded);
                    }
                };
            } else {
                info!(log, "{}", loaded);
            };
            Ok(())
        }
        Err(e) => {
            if dump {
                let yaml = format!("error: \"{}\"", e);
                match snapshot_to {
                    None => {
                        println!("{}", yaml);
                    }
                    Some(file) => {
                        let _ = fs::write(file, yaml);
                    }
                }
            } else {
                error!(log, "{}", e);
            }
            Err(e)
        }
    }
}



#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn statementlog_deser_test() {
        assert!(STATEMENT_LOG_TBL.table_name.len() > 1);
        assert!(STATEMENT_LOG_TBL.description.len() > 15);
        assert!(STATEMENT_LOG_TBL.columns.len() > 15);
    }

    #[test]
    fn config_log_deser_test() {
        assert!(crate::CONFIG_APP.len() > 15);
    }

}