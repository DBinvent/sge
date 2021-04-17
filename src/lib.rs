#[macro_use]
extern crate slog;
use lazy_static::lazy_static;

extern crate semver;

mod dbm_dbc;
mod dbm_log;
pub mod dbm_yml;

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

use self::yaml_rust::Yaml;
use openssl::error::ErrorStack;




pub const CONFIG_DEF_YML: &str = "
name: rdbm
version: \"1.0\"
author: DBInvent
about: \"SQL script tracker and DB deployment by schema definition for Postgres. \
    Execution order is: (P)->(S)->(V)->(R)->(after) \
    i.e. (P)reDeploy, (S)chema yaml, (V)ersion, (R)e-runable, (a)fter migration. \
    The (R) scripts will check a file hashsum and run if the file changed. \
    Other scripts will check for version and execute if version absent in target DB. \
    The 'after.sql' and 'after_each.sql' (names are configurable) will try always execute, but wont record in the history. \
    see more options bellow.\"
group:
    - db:
        args: [db_name, db_host, db_port, db_user, db_password, db_sslmode, db_sslrootcert, db_sslcert, db_sslkey]
    - logdb:
        args: [logdb_name, logdb_host, logdb_port, logdb_user, logdb_password, logdb_sslmode, logdb_sslrootcert, logdb_sslcert, logdb_sslkey]
    - deploy:
        args: [schema_deploy, version_deploy, rr_deploy]

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
    - schema_regxp:
        long: schema_regxp
        default_value: \"*\"
        help: Scan schemas
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
    - version_checksum:
        long: version_checksum
        default_value: \"false\"
        help: Fail on deployed (P)re and (V)ersion scripts checksum mismatch
        possible_values: [yes, no, ok, y, \"1\", \"true\", \"false\"]
    - rr_deploy:
        long: rr_deploy
        default_value: \"*\"
        help: Scan for files to perform deploy (R)e-runable scripts. Found file with suitable versions will check for hashsum and if file absent or changes detected, than script will execute.
    - schema:
        long: schema
        default_value: public
        help: The deployment history 'table' schema location
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
    - fail_safe:
        long: fail_safe
        default_value: \"false\"
        help: Fail the app if applied file different the file provided
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
        help: The target is a database name, it's an empty string (if the log history table on same database); OR environment(dev|prod)+host(node)+database for orchestration configuration
    - db_catalog:
        long: db_catalog
        takes_value: true
        help: The name of set of DB schemas / module / microservice
    - version_deploy:
        long: version_deploy
        default_value: \"*\"
        help: Perform Deploy (V)ersion files up to the target version (* means all)
    - dry_run:
        long: dry_run
        default_value: \"false\"
        help: Generate all SQL, but do not run on DB, also log all attempts to script run
        possible_values: [yes, no, ok, y, \"1\", \"true\", \"false\"]
    - dry_run_to:
        long: dry_run_to
        takes_value: true
        help: The name of file to dump the generated SQL script
    - config_file:
        long: config
        short: c
        takes_value: true
        help: Load all params from config file, all other command line params will be ignored!
    - after:
        long: after
        default_value: \"after.sql\"
        takes_value: true
        help: Try the find and run the script after successful migration execution. Do not fail if not found. This script execution wont log into history table.
    - after_each:
        long: after_each
        default_value: \"after_each.sql\"
        takes_value: true
        help: Try to find and run the script after each 'V' or 'R' successful execution. Do not fail if not found. This script execution wont log into history table.


subcommands:
    - migrate:
        about: Perform migration to version_deploy or all possible scripts
    - info:
        about: Return deployment status, schema dump and schema parsed
    - prepare:
        about: Create a history table use to track scripts executions

";

pub(crate) const BASELINE: &str = "baseline";

lazy_static! {
    pub static ref CONFIG_YAML: Yaml = match YamlLoader::load_from_str(crate::CONFIG_DEF_YML) {
        Ok(y) =>
            if y.len() > 0 {
                y[0].clone()
            } else {
                panic!("zero def config");
            },
        Err(e) => {
            panic!("wrong config: {}", e);
        }
    };
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
    let mut client = connect("logdb_", &cfg)?;
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
    let mut dbc = crate::dbm_dbc::load_info_schema(log_client, cfg)?;
    let yschemas = crate::dbm_yml::load_all(cfg)?;
    let mut t = log_client.transaction().unwrap(); // begin self install if not exists
    let err = {
        match STATEMENT_LOG_TBL.deploy(&mut dbc, &mut t, &schema, cfg, BASELINE, log) {
            Ok(exec) => {
                if exec {
                    debug!(log, "self deployed");
                    if let Err(e) = StatementLog::new(cfg)
                        .instance('S', &BASELINE.into(), &BASELINE.into())
                        .persist(None, &mut t)
                    {
                        Some(e)
                    } else { None }
                } else { None }
            }
            Err(e) => Some(e),
        }
    };
    if let Some(e) = err {
        warn!(log, "rollback on fail self deploy");
        if let Err(e) = t.rollback() {
            warn!(log, "rollback on fail self deploy failed: {}", e);
        }
        return Err(e);
    };
    let res = if prepare_only {
        Ok(())
    } else {
        // select table for update -- row level lock, revert changes
        let logs = { StatementLog::load_all(&mut t, cfg)? };
        let pgsrv = format!("/tmp/.rdbm_{}.conf", cfg.get("db_name", ""));
        let res = match deploy_pre_files(&logs, &yschemas, pgsrv.as_str(), &cfg, &mut t, log) {
            Ok(_) => {
                match deploy_all_schemas(&yschemas, &mut dbc, &cfg, &mut t, log) {
                    Ok(_) => deploy_all_files(&logs, &yschemas, pgsrv.as_str(), &cfg, &mut t, log),
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error)
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
            "host", "port", "user", "password",
            "sslmode", "sslrootcert", "sslcert", "sslkey",
        ];
        for name in names.iter() {
            cfg.get_pow(&mut content, "db_", name, "\n");
        }
        if let Err(e) = fs::write(pgsrv, format!("[{}]\n{}", cfg.get("db_name", ""), content)) {
            return Err(format!("creating PGSERVICEFILE: {}: {}", pgsrv, e));
        }
    }
    let draft = StatementLog::new(cfg);
    // deploy versions
    for (v, file) in &yschemas.pre {
        draft
            .instance('P', v, file)
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
            .instance('V', v, file)
            .deploy(&logs, pgsrv, cfg, t, log)?;
        draft.after("after_each", yschemas, &logs, pgsrv, cfg, t, log)?;
    }
    // deploy re-runable
    for (v, file) in &yschemas.re_runnable {
        draft
            .instance('R', v, file)
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
    let mut client = connect("db_", &cfg)?;
    {
        let stl = StatementLog::new(cfg);
        let mut t = client.transaction().unwrap();
        let mut do_persist = false;
        for (ver, (file, ysv)) in &yschemas.schemas {
            for (s, ys) in ysv {
                for (_, yt) in &ys.tables {
                    match yt.deploy(dbc, &mut t, &s, &cfg, &ys.file, log) {
                        Ok(ok) => { if ok { do_persist = true; }}
                        Err(e) => {
                            err = Some(e);
                            break;
                        }
                    }
                }
                if let Some(_) = &err { break; }
            }
            if do_persist {
                if let Err(e) = stl.instance('S', ver, file).persist(err.clone(), lt) {
                    err = Some(e);
                }
            }
            if let Some(_) = &err { break; }
        }
        let mut do_persist = false;
        if let None = &err {
            // deploy FK
            for (ver, (file, ysv)) in &yschemas.schemas {
                for (s, ys) in ysv {
                    for (_, yt) in &ys.tables {
                        match yt.deploy_fk( &yschemas, dbc, &mut t, &s, &cfg, &ys.file, log) {
                        Ok(ok) => { if ok { do_persist = true; }}
                            Err(e) => {
                                err = Some(e);
                                break;
                            }
                        }
                    }
                    if let Some(_) = &err { break; }
                }
                if do_persist {
                    if let Err(e) = stl.instance('S', ver, file).persist(err.clone(), lt) {
                        err = Some(e);
                    }
                }
                if let Some(_) = &err { break;}
            }
        }
        if let Some(_) = &err {
            let _ = t.rollback();
        } else {
            let _ = t.commit();
        }
    }
    let _ = client.close();
    match err {
        None => Ok(()),
        Some(e) => Err(e),
    }
}

#[inline]
fn connect(prefix: &str, cfg: &ConfigMap) -> Result<Client, String> {
    let mut override_prefix = false;
    if prefix == "logdb_" {
        if let None = cfg.get_opt("logdb_name") {
            override_prefix = true;
        }
    };
    let prefix = if override_prefix { "db_" } else { prefix };
    let mut url = String::new();
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
        cfg.get_pow(&mut url, prefix, name, "&");
    }
    let db = format!(
        "postgresql:///{}?{}",
        cfg.get(format!("{}name", prefix).as_str(), ""),
        url
    );
    match if cfg.is_ssl() {
        let connector = match make_tls_connector(&cfg.get(prefix, "sslcert")) {
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
        Err(err) => Err(format!("Connect to Postgres [{}]: \n{} ", db, err)),
    }
}

#[inline]
fn make_tls_connector(cert: &String) -> Result<MakeTlsConnector, ErrorStack> {
    let mut builder = SslConnector::builder(SslMethod::tls())?;
    builder.set_ca_file(cert)?;
    Ok(MakeTlsConnector::new(builder.build()))
}

#[inline]
pub fn load(cfg: ConfigMap, log: &Logger) -> Result<String, String> {
    let mut client = connect("logdb_", &cfg)?;
    let result = { load_impl(&mut client, &cfg, log) };
    let _ = client.close();
    result
}
