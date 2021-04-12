#[macro_use]
extern crate slog;

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


pub(crate) const BASELINE: &str = "baseline";


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
pub fn migrate(cfg: &ConfigMap, log: &Logger) -> Result<(), String> {
    let mut client = connect("logdb", &cfg)?;
    let result = {
         migrate_db(&mut client, cfg, log)
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
fn migrate_db(log_client: &mut Client, cfg: &ConfigMap, log: &Logger) -> Result<(), String> {
    // prepare schema
    let schema = match cfg.get_opt("schema") {
        Ok(s) => {
            if s.as_bytes()[s.len() - 1] != b'.' {
                format!("{}.", s)
            } else {
                s.clone()
            }
        }
        Err(e) => "public.".into()
    };
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

    // select table for update -- row level lock, revert changes
    let logs = {
         StatementLog::load_all(&mut t,  &schema, &STATEMENT_LOG_TBL.table_name, cfg)?
    };
    let pgsrv = "~/.rdbm.conf";
    let res = match deploy_all_schemas(&yschemas, &mut dbc, &cfg, &mut t, log) {
       Ok(_) => {
           deploy_all_files(&logs, &yschemas, pgsrv, &cfg, &mut t, log)
       }
       Err(error) => {
           Err(error)
       }
    };
    if let Ok(pwd) = cfg.get_opt("db_password") {
        // fs::remove_file(pgsrv);
    }
    t.commit();
    res
}

#[inline]
/// deploy changes by files
fn deploy_all_files(logs: &LS_TYPE, yschemas: &FilesMap, pgsrv: &str, cfg: &ConfigMap, t: &mut Transaction, log: &Logger)  -> Result<(), String> {
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
    let draft = StatementLog::new(cfg);
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
fn deploy_all_schemas(yschemas: &FilesMap, dbc: &mut IS_TYPE, cfg: &ConfigMap, lt: &mut Transaction, log: &Logger) -> Result<(), String> {
    let db_name = cfg.get("db_name", "");
    let mut err = None;
    let mut client = connect("db", &cfg)?;
    {
        let mut t = client.transaction().unwrap();
        for (ver, ysv) in &yschemas.schemas {
            // StatementLog::new()
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
            if let Some(_) = &err {
                break;
            }
            match &err {
                None => {
                    StatementLog::save_ok(&db_name, lt);
                }
                Some(e) => {
                    StatementLog::save_err(&db_name, &e.to_string(), lt);
                }
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

#[cfg(test)]
mod tests {
    use std::collections::hash_map::RandomState;

    use crate::dbm_dbc::STable;

    use super::*;

    #[test]
    fn connect_test() {
        let mut cfg = ConfigMap::new("db_port", "5433");
        cfg.set("db_name", "vk");
        cfg.set("db_user", "vk");
        match connect("db", &cfg) {
            Ok(mut c) => {
                let dbc = crate::dbm_dbc::load_info_schema(&mut c, &cfg).unwrap();
                assert!(dbc.len() > 0);
                match dbc.get("public") {
                    None => { assert!(false); }
                    Some(p) => {
                        assert!(p.len() > 0);
                    }
                }
                println!("{:?}", dbc);
            }
            Err(e) => {
                assert!(false, "{}", e);
            }
        }
    }
}
