#[macro_use]
extern crate slog;
use lazy_static::lazy_static;

extern crate semver;

pub mod dbm_yml;
mod dbm_dbc;
mod dbm_log;

use crate::dbm_log::*;
use crate::dbm_yml::*;
use postgres::{NoTls, Client, Transaction};
use slog::Logger;
use crate::dbm_dbc::IS_TYPE;
use std::sync::{Mutex, Arc};
use std::borrow::BorrowMut;
use std::fs;

extern crate yaml_rust;

use yaml_rust::YamlLoader;

use self::yaml_rust::{ScanError, Yaml};
use std::ops::Deref;





pub const CONFIG_DEF_YML: &str = "
name: rdbm
version: \"1.0\"
author: DBInvent
about: SQL script tracker and DB deployment by schema definition for Postgres
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
        help: password
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
        help: pwd
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
    - ignore_order:
        long: ignore_order
        default_value: \"false\"
        help: Perform Versions order execution
        possible_values:
            - \"true\"
            - \"false\"
    - schema_regxp:
        long: schema_regxp
        default_value: \"*\"
        help: Scan schemas
    - schema_deploy:
        long: schema_deploy
        default_value: \"*\"
        help: Scan for files to perform deploy (S)chema scripts, i.e. target schema files to deploy, up to the target version (* means all)
    - version_checksum:
        long: version_checksum
        default_value: \"false\"
        help: Fail on deployed scripts version checksum mismatch
        possible_values:
            - \"true\"
            - \"false\"
    - rr_deploy:
        long: rr_deploy
        default_value: \"*\"
        help: Scan for files to perform deploy (R)e-runable scripts
    - schema:
        long: schema
        default_value: public
        help: deployment history statement_log location
    - source_dir:
        long: source_dir
        default_value: \"./\"
        help: Directory scripts location, use <source_recur> to scan sub dir
    - source_recur:
        long: source_recur
        default_value: \"true\"
        help: Scan Directory recursively
        possible_values:
            - \"true\"
            - \"false\"
    - fail_safe:
        long: fail_safe
        default_value: \"false\"
        help: Fail the app if applied file different the file provided
        possible_values:
            - \"true\"
            - \"false\"
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
        help: generate, but do not run schema, also log all operations
        possible_values:
            - \"true\"
            - \"false\"
    - dry_run_to:
        long: dry_run_to
        takes_value: true
        help: The name of file to dump the generated SQL script


subcommands:
    - migrate:
        about: Perform migration to version_deploy or all posible
    - info:
        about: Return deployment status
    - prepare:
        about: Create a history table to track scripts executions
:
";

pub(crate) const BASELINE: &str = "baseline";

lazy_static! {
	pub static ref CONFIG_YAML: Yaml = match YamlLoader::load_from_str(crate::CONFIG_DEF_YML) {
		Ok(y) => if y.len() > 0 {
            y[0].clone()
		}else{panic!(format!("zero def config"));},
		Err(e) => { panic!(format!("wrong config: {}", e)); }
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
pub fn migrate(cfg: ConfigMap, po: bool, log: &Logger) -> Result<(), String> {
    let mut client = connect("logdb", &cfg)?;
    let result = {
         migrate_db(&mut client, &cfg, po, log)
    };
    client.close();
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
/// - define target versions: from - to, check logs
///    -- psql "host=localhost user=vk password=1 dbname=vk port=5433" -X -1 -f
///       sslmode=verify-full \
///       sslrootcert=/path/to/server-ca.crt \
///       sslcert=/path/to/client.crt \
///       sslkey=/path/to/client.key
/// - migrate 'V'ersion files, save log
/// - define re-run by logs
/// - migrate 'R'e-runable files, save log
/// - run post_run files
#[inline]
fn migrate_db(log_client: &mut Client, cfg: &ConfigMap, po: bool, log: &Logger) -> Result<(), String> {
    // prepare schema
    let schema = cfg.get("schema", "public");
    let mut dbc = crate::dbm_dbc::load_info_schema(log_client, cfg)?;
    let yschemas = crate::dbm_yml::load_all(cfg)?;
    let mut t = log_client.transaction().unwrap(); // begin
    // self install if not exists
    let err = {
        match STATEMENT_LOG_TBL.deploy(&mut dbc, &mut t, &schema, cfg, BASELINE, log) {
            Ok(_) => None,
            Err(e) => Some(e)
        }
    };
    if let Some(e) = err {
        t.rollback();
        return Err(e);
    };
    let res = if !po {
        let log_st = format!("{}.{}", &schema, &STATEMENT_LOG_TBL.table_name);
        // select table for update -- row level lock, revert changes
        let logs = {
            StatementLog::load_all(&mut t, &log_st, cfg)?
        };
        let pgsrv = format!("~/.rdbm_{}.conf", cfg.target());
        let res = match deploy_all_schemas(&yschemas, &log_st, &mut dbc, &cfg, &mut t, log) {
            Ok(_) => {
                deploy_all_files(&logs, &yschemas, &log_st, pgsrv.as_str(), &cfg, &mut t, log)
            }
            Err(error) => {
                Err(error)
            }
        };
        if let Ok(pwd) = cfg.get_opt("db_password") {
            let _ = fs::remove_file(pgsrv);
        }
        res
    } else { Ok(()) };
    t.commit();
    res
}

#[inline]
/// deploy changes by files
fn deploy_all_files(logs: &LS_TYPE, yschemas: &FilesMap, log_st: &String, pgsrv: &str, cfg: &ConfigMap, t: &mut Transaction, log: &Logger)  -> Result<(), String> {
    if let Ok(pwd) = cfg.get_opt("db_password") {
        fs::write(pgsrv, format!( "[rdbm]\nhost={}\n{}\npassword={}\n{}\n{}\n{}\n{}\n",
            cfg.get("db_host", ""),
            cfg.get_po("db_", "user").unwrap_or("".into()),
            pwd,
            cfg.get_po("db_", "sslmode").unwrap_or("".into()),
            cfg.get_po("db_", "sslrootcert").unwrap_or("".into()),
            cfg.get_po("db_", "sslcert").unwrap_or("".into()),
            cfg.get_po("db_", "sslkey").unwrap_or("".into()),
            ));
    }
    let draft = StatementLog::new(log_st, cfg);
    // deploy versions
    for (v, file) in &yschemas.versions {
        draft.instance('S', v, file).deploy(&logs, pgsrv, cfg, t, log)?;
    }
    // deploy re-runable
    for (v, file) in &yschemas.re_runnable {
        draft.instance('R', v, file).deploy(&logs, pgsrv, cfg, t, log)?;
    }
    // deploy deploy after
    for (v, file) in &yschemas.after {
        draft.instance('A', v, file).deploy(&logs, pgsrv, cfg, t, log)?;
    }
    Ok(())
}

#[inline]
/// deploy schema by definitions - declaratives
fn deploy_all_schemas(yschemas: &FilesMap, log_st: &String, dbc: &mut IS_TYPE, cfg: &ConfigMap, lt: &mut Transaction, log: &Logger) -> Result<(), String> {
    let mut err = None;
    let mut client = connect("db", &cfg)?;
    {
        let mut stl = StatementLog::new(log_st, cfg);
        let mut t = client.transaction().unwrap();
        for (ver, (file, ysv)) in &yschemas.schemas {
            for (s, ys) in ysv {
                for (_, yt) in &ys.tables {
                    if let Err(e) = yt.deploy(dbc, &mut t, &s, &cfg, &ys.file, log) {
                        err = Some(e);
                        break;
                    }
                }
                if let Some(_) = &err {
                    break;
                }
            }
            if let Err(e) = stl.instance('S', ver, file).persist(err.clone(), lt) {
                err = Some(e);
            }
            if let Some(_) = &err {
                break;
            }
        }
        if let Some(_) = &err {
            t.rollback();
        } else {
            t.commit();
        }
    }
    let _ = client.close();
    match err {
        None => Ok(()),
        Some(e) => Err(e)
    }
}

#[inline]
fn connect(prefix: &str, cfg: &ConfigMap) -> Result<Client, String> {
    let mut override_prefix = false;
    if prefix == "logdb " {
        if let Err(_) = cfg.get_po(prefix,"_name") {
            override_prefix = true;
        }
    };
    let prefix = if override_prefix { "db" } else { prefix };
    let db = format!("postgresql:///{}?user={}&password={}&port={}&host={}",
                     cfg.get_po(prefix,"_name")?, cfg.get_po(prefix, "_user")?, cfg.get_po(prefix, "_password")?,
                     cfg.get_po(prefix, "_port")?, cfg.get_po(prefix, "_host")?);
    // TODO add TLS_MODE
    match Client::connect(db.as_str(), NoTls) {
        Ok(client) => Ok(client),
        Err(err) => {
            return Err(format!("ERROR on Connect to Postgres [{}]: \n{} ", db, err));
        }
    }
}

#[inline]
pub fn load(cfg: ConfigMap, log: &Logger) -> Result<String, String> {
    let mut client = connect("logdb", &cfg)?;
    let result = {
         load_impl(&mut client, &cfg, log)
    };
    client.close();
    result
}
