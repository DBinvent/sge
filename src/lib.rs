#[macro_use]
extern crate slog;
use lazy_static::lazy_static;

extern crate semver;
extern crate yaml_rust;

pub mod dbm_cfg;
mod dbm_dbc;
mod dbm_log;
pub mod dbm_yml;
pub mod license;
pub mod i18n;
pub mod dbm_etl;

use crate::dbm_dbc::InfoSchemaType;
use crate::dbm_log::*;
use crate::dbm_yml::*;
use openssl::ssl::{SslConnector, SslMethod};
use postgres::{Client, NoTls, Transaction};
use postgres_openssl::MakeTlsConnector;
use slog::Logger;
use std::fs;

use yaml_rust::YamlLoader;

use crate::dbm_cfg::ConfigMap;
use openssl::error::ErrorStack;
use postgres::error::SqlState;
use std::collections::HashMap;
use std::fmt::Write;

const STATEMENT_LOG_TABLE_YML: &'static str = include_str!("hl_table.yaml");

static YAML_SCHEMA: &'static str = include_str!("schema.yaml");

/// TODO:  static JSON_SCHEMA: &'static str = include_str!("schema.json");

pub(crate) const BASELINE: &str = "baseline";

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
    pub static ref CONFIG_APP: HashMap<String, Option<String>> =
        match YamlLoader::load_from_str(crate::i18n::cl_config()) {
            Ok(yaml) => {
                let mut defs = HashMap::new();
                for args in yaml[0]["args"].as_vec().unwrap() {
                    for (k, v) in args.as_hash().unwrap() {
                        if let Some(k) = k.as_str() {
                            if k.as_bytes()[0] == b'$' {
                                let k = k[1..].to_string();
                                let v = match std::env::var(&k.to_uppercase()) {
                                    Ok(v) => Some(v), Err(_) => None
                                };
                                defs.insert(k, v);
                            } else {
                                defs.insert(k.into(), v["default_value"].as_str().map(|v| v.into()));
                            }
                        }
                    }
                }
                defs
            }
            Err(e) => {
                panic!("wrong config: {}", e);
            }
        };

    pub static ref SCHEMA_YAML: Vec<yaml_rust::Yaml> = YamlLoader::load_from_str(crate::YAML_SCHEMA).expect("wrong schema");

    pub static ref TR: crate::i18n::Translate = crate::i18n::Translate::new();
}

/// Main entry point
/// Relation (PostgreSQL) DB Migration Management tool (inspired by flyway while it was free)
pub fn migrate(cfg: ConfigMap, prepare_only: bool, log: &Logger) -> Result<(), String> {
    trace!(log, "migrate prepare_only {}, with logdb {}, is_logdb: {}, is_primary: {}", prepare_only, cfg.get("logdb"), cfg.is_logdb(), cfg.is_primary());
    let mut client = connect(if cfg.is_primary() {"logdb_"} else {"db_"}, &cfg, None, "logdata", log)?;
    let mut copy_log = if !cfg.is_primary() && cfg.is_logdb() {
        trace!(log, "migrate connecting COPYLOG ...");
        Some(connect("logdb_", &cfg, None, "copylog", log)?)
    } else { None};

    let result = { migrate_db(&mut client, &mut copy_log, &cfg, prepare_only, log) };
    match client.close() {
        Ok(_) => {}
        Err(_) => {}
    }
    if let Some(c) = copy_log {
        match c.close() {
            Ok(_) => {}
            Err(_) => {}
        }
    }
    result
}

/// Steps on execute:
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
    copy_log_client: &mut Option<Client>,
    cfg: &ConfigMap,
    prepare_only: bool,
    log: &Logger,
) -> Result<(), String> {
    // prepare schema
    let schema = cfg.schema();

    let log_db_name = if cfg.is_primary() {
        cfg.get_opt("logdb_name").unwrap_or(cfg.get("db_name"))
    } else {
        cfg.get("db_name")
    };
    let mut dbc = crate::dbm_dbc::load_info_schema(log_db_name.as_str(), log_client)?;
    let files = crate::dbm_yml::load_all(cfg, log)?;
    let mut t = log_client.transaction().unwrap(); // begin self install if not exists
    let mut err = YTable::baseline_deploy(&mut dbc, &mut t, &schema, cfg, "primary", log);

    let mut ct = match copy_log_client {
        None => {None}
        Some(lc) => {
            if err.is_none() {
                // debug!(log, "begin self install if not exists on log copy instance");
                // TODO reuse loaded?
                let mut dbc = crate::dbm_dbc::load_info_schema(cfg.get("logdb_name").as_str(), lc)?;
                let mut t = lc.transaction().unwrap();
                if !cfg.is_primary() {
                    err = YTable::baseline_deploy(&mut dbc, &mut t, &schema, cfg, "copy", log);
                }
                Some(t)
            } else { None }
        }
    };
    if let Some(e) = err {
        warn!(log, "rollback on fail self deploy: {}", e);
        if let Err(e) = t.rollback() {
            warn!(log, "rollback on fail self deploy failed: {}", e);
        }
        return Err(e);
    };
    let pgsrv = format!("/tmp/.rdbm_{}.conf", cfg.get("db_name"));
    let res = if prepare_only {
        Ok(vec![])
    } else {
        // select table for update -- row level lock, revert changes
        let logs = { StatementLog::load_all(&mut t, cfg.get_bool("dry_run", true), cfg)? };

        let db_name = cfg.get("db_name");
        let mut dbc = if log_db_name != db_name {
            let mut client = connect("db_", &cfg, None, "loading info schema",log)?;
            let dbc = crate::dbm_dbc::load_info_schema(db_name.as_str(),&mut client)?;
            let _ = client.close();
            debug!(log, "loading info schema disconnected");
            dbc
        } else { dbc };
        let mut client = connect("db_", &cfg, None, "migrate",log)?;
        let r = migrate_all_versions(
            &mut client, // main target db client
            &mut dbc, // loaded info schema
            &logs, // loaded log history
            &files, // list of parsed files
            pgsrv.as_str(), // name of psql config file
            cfg,  // config
            &mut t, // log client transaction
            &mut ct, // copy_log_client
            log); // console logging
        let _ = client.close();
        r
    };
    let ret = match t.commit() {
        Ok(()) => {
            let _ = StatementLog::new(cfg).after("after", &files, &HashMap::new(), pgsrv.as_str(), cfg, None,  log)?;
            match &res {
                Ok(_) => {
                    debug!(log, "all done");
                    Ok(())
                },
                Err(e) => Err(e.to_string()),
            }
        },
        Err(e) => {
            error!(log, "commit history failed: {}", e);
            match &res {
                Ok(_) => Err(format!("commit history failed: {}", e)),
                Err(ee) => Err(format!("{}. Also, commit history failed: {}", e, ee)),
            }
        }
    };
    // copy_log_client
    if let Some(t) = ct {
        match ret {
            // log it
            Ok(_) => {
                match t.commit() {
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
            Err(_) => {
                match t.rollback() {
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        }
    }

    if cfg.is_pgsrv() {
        let _ = fs::remove_file(pgsrv.as_str());
    }
    ret
}

/// version level ordering
fn migrate_all_versions(
    client: &mut Client,
    dbc: &mut InfoSchemaType,
    logs: &LogSchemaType,
    files: &FilesMap,
    pgsrv: &str,
    cfg: &ConfigMap,
    t: &mut Transaction,
    clog_t: &mut Option<Transaction>, // copy_log_client
    log: &Logger,
) -> Result<Vec<i32>, String> {
    let mut res = Vec::new();
    let draft = StatementLog::new(cfg);
    for ver in files.all_versions() {
        info!(log, "Deploying version: {}", ver);
        res.push(deploy_pre_files(&logs, &ver, &files, pgsrv, &cfg, t, clog_t, log)?);
        res.push(deploy_all_schemas(&logs, client, &ver, &files, dbc, &cfg, t, clog_t, log)?);
        res.push(deploy_all_files(&logs, &ver, &files, pgsrv, &cfg, t, clog_t, log)?);
        let _ = draft.after("after_each", files, &logs, pgsrv, cfg, Some(t), log)?;
    }
    Ok(res)
}

#[inline]
/// deploy changes by files
fn deploy_pre_files(
    logs: &LogSchemaType,
    ver: &FileVersion,
    files: &FilesMap,
    pgsrv: &str,
    cfg: &ConfigMap,
    t: &mut Transaction,
    clog_t: &mut Option<Transaction>, // copy_log_client
    log: &Logger,
) -> Result<i32, String> {
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
    Ok(
        if let Some(file) = files.pre.get(ver) {
            let id = draft
                .instance('P', &ver.s, &cfg.get("source_dir"), file)
                .deploy(&logs, pgsrv, cfg, Some(t), clog_t, true, log)?;
            draft.after("after_each", files, &logs, pgsrv, cfg, Some(t), log)?;
            id
        } else { 0 }
    )
}

#[inline]
/// deploy changes by files
fn deploy_all_files(
    logs: &LogSchemaType,
    ver: &FileVersion,
    files: &FilesMap,
    pgsrv: &str,
    cfg: &ConfigMap,
    t: &mut Transaction,
    clog_t: &mut Option<Transaction>, // copy_log_client
    log: &Logger,
) -> Result<i32, String> {
    let mut id = 0;
    let draft = StatementLog::new(cfg);
    // deploy versions
    if let Some(file) = &files.versions.get(ver) {
        id = draft
            .instance('V', &ver.s, &cfg.get("source_dir"), file)
            .deploy(&logs, pgsrv, cfg, Some(t), clog_t, true, log)?;
    }
    // deploy re-runable
    if let Some(file) = &files.re_runnable.get(ver) {
        id = draft
            .instance('R', &ver.s, &cfg.get("source_dir"), file)
            .deploy(&logs, pgsrv, cfg, Some(t), clog_t, true, log)?;
    }
    Ok(id)
}

#[inline]
/// deploy schema by definitions - declaratives
fn deploy_all_schemas(
    logs: &LogSchemaType,
    client: &mut Client,
    ver: &FileVersion,
    files: &FilesMap,
    dbc: &mut InfoSchemaType,
    cfg: &ConfigMap,
    lt: &mut Transaction,
    clog_t: &mut Option<Transaction>, // copy_log_client
    log: &Logger,
) -> Result<i32, String> {
    let mut id = 0;
    let mut err = None;
    // let mut client = connect("db_", &cfg, None, log)?;
    {
        let stl = StatementLog::new(cfg);
        let etl = dbm_etl::Etl::from(cfg)?;
        let table_tr = cfg.get("schema_transaction") == "table";
        let mut do_persist = false;
        // for (ver, (file, ysv)) in &files.schemas
        if let Some((file, ysv)) = &files.schemas.get(ver) {
            match client.transaction() {
                Ok(mut t) => {
                    // let mut t = client.transaction().unwrap();
                    for (s, ys) in ysv {
                        for yt in &ys.tables.list {
                            match yt.deploy(dbc, &mut t, &s, &cfg, &ys.file, log) {
                                Ok(ok) => {
                                    if ok {
                                        do_persist = true;
                                        if table_tr {
                                            if let Err(ec) = t.batch_execute("COMMIT; BEGIN") {
                                                do_persist = true;
                                                err = Some(format!(
                                                    "committing change on {}.{} with {}",
                                                    s, yt.table_name, ec
                                                ));
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
                                match yt.deploy_fk(ver, &files, dbc, &mut t, &s, &cfg, &ys.file, log)
                                {
                                    Ok(ok) => {
                                        if ok {
                                            do_persist = true;
                                            if table_tr {
                                                if let Err(ec) = t.batch_execute("COMMIT; BEGIN") {
                                                    do_persist = true;
                                                    err = Some(format!(
                                                        "committing change on {}.{} with {}",
                                                        s, yt.table_name, ec
                                                    ));
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
                    if err.is_none() {
                        match etl.deploy_all(&ysv, logs, dbc, &mut t, lt, clog_t, &stl, log) {
                            Ok(p) => do_persist = do_persist || p,
                            Err(e) => {
                                do_persist = true;
                                err = Some(e);
                            }
                        }
                    }

                    if table_tr || !do_persist {
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
                Err(e) => err = Some(format!("creating transaction for {} {}", file, e)),
            }
            if do_persist {
                match stl.instance('S', &ver.s, &cfg.get("source_dir"), file)
                    .persist(err.clone(), Some(lt), clog_t) {
                    Ok(i) => {
                        id = i;
                    }
                    Err(e) => {
                        err = Some(e);
                    }
                }
            }
        }
    }
    // let _ = client.close();
    match err {
        None => Ok(id),
        Some(e) => Err(e),
    }
}

#[inline]
fn connect(
    prefix: &str,
    cfg: &ConfigMap,
    create: Option<bool>,
    module: &str,
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
            cfg.get_def(
                format!("create{}user", prefix).as_str(),
                cfg.get(format!("{}user", prefix).as_str())
            )
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
        debug!(log, "{}: create db connect: {}", module, log_url);
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
    debug!(log, "{}: connecting: {} {}", module, log_url, dbname);
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
                        || code == &SqlState::INVALID_AUTHORIZATION_SPECIFICATION
                        || code.code().starts_with("0800")
                    {
                        if cfg.get_bool("createdb", false) {
                            return create_connect(
                                dbname.as_str(),
                                prefix,
                                cfg,
                                module,
                                log,
                            );
                        } else {
                            error!(log, "NO DB: {}, create or use --createdb=y", dbname);
                        }
                    } else {
                        debug!(log, "{}: connection error code: {:?}", module, code);
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
    dbname: &str,
    prefix: &str,
    cfg: &ConfigMap,
    module: &str,
    log: &Logger,
) -> Result<Client, String> {
    info!(
        log,
        "NO DB name [{}] or user found, trying to connect as super user ...", dbname
    );
    let mut client = connect(prefix, cfg, Some(true), module, log)?;
    let owner = match cfg.get_opt(format!("{}user", prefix).as_str()) {
        None => "".to_string(),
        Some(user) => {
            debug!(log, "testing for user: {}", user);
            create_role(&mut client, &user, prefix, cfg, log)?;
            format!(" OWNER {}", user)
        }
    };
    match connect(prefix, cfg, Some(false), module, log) {
        Ok(c) => { let _ = c.close(); }
        Err(e) => {
            debug!(log, "{} testing connection: {}", dbname, e);
            info!(log, "NO DB name [{}] found, trying to create...", dbname);
            let _ = client
                .batch_execute(format!("CREATE DATABASE {} {}", dbname, owner).as_str())
                .map_err(|e| format!("Creating DB [{}] by postgres: {} ", dbname, e))?;
        }
    }
    let _ = client.close();
    info!(log, "DB/user [{}] created, reconnecting", dbname);
    connect(prefix, cfg, Some(false), module, log)
}

fn create_role(
    db: &mut Client,
    user: &String,
    prefix: &str,
    cfg: &ConfigMap,
    log: &Logger,
) -> Result<(), String> {
    let row = db
        .query_one(
            "SELECT count(rolname) FROM pg_roles WHERE rolname = $1",
            &[user],
        )
        .map_err(|e| format!("loading role [{}] {}", user, e))?;
    let users: i64 = row.get(0);
    if users == 0 {
        info!(log, "NO user {} found, trying to create ...", user);
        let pwd = match cfg.get_opt(format!("{}password", prefix).as_str()) {
            None => "".to_string(),
            Some(pwd) => format!(" PASSWORD '{}'", pwd),
        };
        let _ = db
            .batch_execute(format!("CREATE ROLE {} WITH LOGIN {}", user, pwd).as_str())
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
    let mut log_client = connect("logdb_", &cfg, None, "loading logs",log)?;
    let mut db_client = connect("db_", &cfg, None, "loading data",log)?;
    let etl = dbm_etl::Etl::from(&cfg)?;
    let result = if dump {
        LoadResult::load(&etl, &mut db_client, log)?.format(etl.yaml_format)
    } else {
        StatementLog::load_formatted(&cfg, &mut log_client)
    };
    let _ = db_client.close();
    let _ = log_client.close();
    etl.dump_loaded(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn statementlog_deser_test() {
        assert!(STATEMENT_LOG_TBL.table_name.len() > 1);
        assert!(STATEMENT_LOG_TBL.description.len() > 15);
        assert!(STATEMENT_LOG_TBL.columns.len() > 15);
        let last = STATEMENT_LOG_TBL.columns.len()-1;
        let times = STATEMENT_LOG_TBL.columns.list[last - 1].clone();
        assert_eq!(times.name, "times".to_string());
        assert_eq!(times.default_value.clone()
            .unwrap_or("".to_string()), "1".to_string());

    }

    #[test]
    fn config_log_deser_test() {
        assert!(crate::CONFIG_APP.len() > 15);
    }
}
