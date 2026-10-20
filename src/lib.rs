//! sge (Schema Guard Enterprise): no unsafe code - the compiler holds it to that
#![forbid(unsafe_code)]

#[macro_use]
extern crate slog;
use lazy_static::lazy_static;

extern crate semver;
extern crate yaml_rust2;

pub mod dbm_cfg;
mod dbm_log;
mod dbm_files;
/// [git] git repository as a script source - see `source_git`
mod dbm_git;
/// [yt] rolling migration across a YT Cluster, node by node
pub mod dbm_yt;
pub mod i18n;
pub mod dbm_etl;

use crate::dbm_log::*;

// #[cfg(feature = "tls")]
// use ssl::{
//     ssl::{SslConnector, SslMethod},
//     error::ErrorStack
// };

// #[cfg(feature = "tls")]
// use postgres_openssl::MakeTlsConnector;

use tokio_postgres::{Client, NoTls, Transaction};
use slog::Logger;

use yaml_rust2::YamlLoader;

use crate::dbm_cfg::ConfigMap;
use tokio_postgres::error::SqlState;
use std::collections::HashMap;
use std::fmt::Write;
use schema_guard_tokio::loader::{InfoSchemaType, load_info_schema};
use schema_guard_tokio::table::Table;
use crate::dbm_files::{FilesMap, FileVersion, load_all};

const STATEMENT_LOG_TABLE_YML: &'static str = include_str!("hl_table.yaml");


// static YAML_SCHEMA: &'static str = include_str!("../sg/src/schema.yaml");

/// TODO:  static JSON_SCHEMA: &'static str = include_str!("schema.json");

pub(crate) const BASELINE: &str = "baseline";

/// Max varchar len for columns name: source, target, msg
pub(crate) const SRC_T_M_LEN: usize = 250;

/// Max varchar len for all other columns names
pub(crate) const SLT_C_LEN: usize = 80;

lazy_static! {
    pub static ref STATEMENT_LOG_TBL: Table =
        match YamlLoader::load_from_str(STATEMENT_LOG_TABLE_YML) {
            Ok(y) => {
                if y.len() > 0 {
                    let y = &y[0];
                    Table::new(&y, y["tableName"].as_str().unwrap(), None).unwrap()
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

    // pub static ref SCHEMA_YAML: Vec<yaml_rust::Yaml> = YamlLoader::load_from_str(crate::YAML_SCHEMA).expect("wrong schema");

    pub static ref TR: crate::i18n::Translate = crate::i18n::Translate::new();
}

/// [sg-tokio] Connect and drive the connection.
///
/// `tokio_postgres::connect` hands back a `(Client, Connection)` pair, and the
/// Connection is a future that must be polled or the client simply never makes
/// progress - queries hang rather than failing, which is the least debuggable
/// outcome available. The sync `postgres` crate hid this by running its own
/// runtime; here it is explicit.
async fn spawn_connect(url: &str) -> Result<Client, tokio_postgres::Error> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move {
        // an error here means the socket died; the next client call reports it
        let _ = connection.await;
    });
    Ok(client)
}

#[cfg(feature = "tls")]
async fn spawn_connect_tls<T>(url: &str, tls: T) -> Result<Client, tokio_postgres::Error>
where
    T: tokio_postgres::tls::MakeTlsConnect<tokio_postgres::Socket> + Send + 'static,
    T::Stream: Send + 'static,
    <T as tokio_postgres::tls::MakeTlsConnect<tokio_postgres::Socket>>::TlsConnect: Send,
    <<T as tokio_postgres::tls::MakeTlsConnect<tokio_postgres::Socket>>::TlsConnect as tokio_postgres::tls::TlsConnect<tokio_postgres::Socket>>::Future: Send,
{
    let (client, connection) = tokio_postgres::connect(url, tls).await?;
    tokio::spawn(async move { let _ = connection.await; });
    Ok(client)
}

/// [sg-tokio] sge configuration expressed as SchemaGuard migration options.
///
/// `is_retry` used to be a parameter on `deploy`; in sg_tokio it is one flag of
/// seven on `MigrationOptions`, and the other six are the destructive-change
/// controls. They all default to FALSE, meaning sg raises rather than silently
/// dropping an index, a trigger or a grant - which is the behaviour sge wants
/// and previously had no way to express.
///
/// Not an sge setting: turning a `with_*` on lets a migration destroy an object,
/// which is not something to leave on in a properties file. On a yt cluster they
/// are the cluster's own `C sg_migration` row (`dbm_yt::sg_migration`, read once
/// in `migrate`), because the cluster's own schema check judges differences by the
/// same options - one setting, set deliberately in the cluster's config. Standalone,
/// SchemaGuard's defaults. `with_ddl_retry` also follows `is_retry`, as before.
fn migration_options(cfg: &ConfigMap) -> schema_guard_tokio::MigrationOptions {
    let sg = cfg.get("sg_migration");
    let on = |name: &str| sg.split(',').any(|o| o.trim().eq_ignore_ascii_case(name));
    schema_guard_tokio::MigrationOptions {
        with_size_cut: on("with_size_cut"),
        with_index_drop: on("with_index_drop"),
        with_trigger_drop: on("with_trigger_drop"),
        exclude_triggers: on("exclude_triggers"),
        with_revoke: on("with_revoke"),
        without_failfast: on("without_failfast"),
        with_ddl_retry: cfg.is_retry() || on("with_ddl_retry"),
    }
}

/// Main entry point
/// Relational (PostgreSQL) database migration management
pub async fn migrate(mut cfg: ConfigMap, prepare_only: bool, log: &Logger) -> Result<(), String> {
    trace!(log, "migrate prepare_only {}, with logdb {}, is_logdb: {}, is_primary: {}", prepare_only, cfg.get("logdb"), cfg.is_logdb(), cfg.is_primary());
    let mut client = connect(if cfg.is_primary() {"logdb_"} else {"db_"}, &cfg, None, "logdata", log).await?;
    let mut copy_log = if !cfg.is_primary() && cfg.is_logdb() {
        trace!(log, "migrate connecting COPYLOG ...");
        Some(connect("logdb_", &cfg, None, "copylog", log).await?)
    } else { None};
    schema_guard_tokio::set_logger(log.clone());
    // [yt] the cluster's SchemaGuard options, for every node the roll reaches -
    // node_cfg clones this map
    if dbm_yt::enabled(&cfg) {
        if let Some(sg) = dbm_yt::sg_migration(&mut client, &cfg).await {
            debug!(log, "[yt] SchemaGuard migration options from the cluster: [{}]", sg);
            cfg.insert(&"sg_migration".to_string(), sg.as_str());
        }
    }
    let result = { migrate_db(&mut client, &mut copy_log, &cfg, prepare_only, log).await };
    // [sg-tokio] tokio_postgres has no Client::close(): the connection task
    // ends when the client drops, so the scope does what close() used to.
    drop(copy_log);
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
async fn migrate_db(
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
    let mut dbc = load_info_schema(log_db_name.as_str(), &mut log_client.transaction().await.map_err(|e| e.to_string())?).await?;
    let files = load_all(cfg, log)?;
    let mut t = log_client.transaction().await.unwrap(); // begin self install if not exists
    let mut err = baseline_deploy(&mut dbc, &mut t, &schema, cfg, "primary", log).await;

    let mut ct = match copy_log_client {
        None => {None}
        Some(lc) => {
            if err.is_none() {
                // debug!(log, "begin self install if not exists on log copy instance");
                // TODO reuse loaded?
                let mut t = lc.transaction().await.map_err(|e| e.to_string())?;
                let mut dbc = load_info_schema(cfg.get("logdb_name").as_str(), &mut t).await?;
                // let mut t = lc.transaction().await.unwrap();
                if !cfg.is_primary() {
                    err = baseline_deploy(&mut dbc, &mut t, &schema, cfg, "copy", log).await;
                }
                Some(t)
            } else { None }
        }
    };
    if let Some(e) = err {
        warn!(log, "rollback on fail self deploy: {}", e);
        if let Err(e) = t.rollback().await {
            warn!(log, "rollback on fail self deploy failed: {}", e);
        }
        return Err(e);
    };
    let res = if prepare_only {
        Ok(vec![])
    } else {
        // select table for update -- row level lock, revert changes
        let logs = { StatementLog::load_all(&mut t, cfg.get_bool("dry_run", true), cfg).await? };

        let db_name = cfg.get("db_name");
        let mut dbc = if log_db_name != db_name {
            let mut client = connect("db_", &cfg, None, "loading info schema",log).await?;
            let dbc = load_info_schema(db_name.as_str(), &mut client.transaction().await.unwrap()).await?;
            debug!(log, "loading info schema disconnected");
            dbc
        } else { dbc };
        let mut client = connect("db_", &cfg, None, "migrate",log).await?;
        let r = migrate_all_versions(
            &mut client, // main target db client
            &mut dbc, // loaded info schema
            &logs, // loaded log history
            &files, // list of parsed files
            cfg,  // config
            &mut t, // log client transaction
            &mut ct, // copy_log_client
            log).await; // console logging
        r
    };
    let ret = match t.commit().await {
        Ok(()) => {
            let _ = StatementLog::new(cfg).after("after", &files, &HashMap::new(), cfg, None,  log).await?;
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
                match t.commit().await {
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
            Err(_) => {
                match t.rollback().await {
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
        }
    }

    ret
}

pub(crate) async fn baseline_deploy(
    dbc: &mut InfoSchemaType,
    t: &mut Transaction<'_>,
    schema: &String,
    cfg: &ConfigMap,
    cause: &str,
    log: &Logger,
) -> Option<String> {
    debug!(log, "starting with self deploy {} for {}", BASELINE, cause);
    let mut baseline = crate::STATEMENT_LOG_TBL.clone();
    baseline.table_name = cfg.history_table_name();
    // [sg-tokio] deploy is async, dropped is_retry, gained MigrationOptions,
    // and the callback must be Send + Sync
    let dr: Box<dyn Fn(Vec<String>) -> Result<(), String> + Send + Sync> =
        Box::new(|input| cfg.dry_run(input));
    match baseline.deploy(dbc, t, &schema, BASELINE,
      if cfg.get_bool("dry_run", false) { Some(dr.as_ref()) } else {None},
      &migration_options(cfg)
    ).await {
        Ok(exec) => {
            if exec {
                debug!(log, "self deploy of: {} complete", BASELINE);
                if let Err(e) = StatementLog::new(cfg)
                    .instance('S', &BASELINE.into(), &"".into(), &BASELINE.into())
                    .persist(None, Some(t), &mut None).await
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
}

/// version level ordering
async fn migrate_all_versions(
    client: &mut Client,
    dbc: &mut InfoSchemaType,
    logs: &LogSchemaType,
    files: &FilesMap,
    cfg: &ConfigMap,
    t: &mut Transaction<'_>,
    clog_t: &mut Option<Transaction<'_>>, // copy_log_client
    log: &Logger,
) -> Result<Vec<i32>, String> {
    let mut res = Vec::new();
    let draft = StatementLog::new(cfg);
    for ver in files.all_versions() {
        info!(log, "Deploying version: {}", ver);
        res.push(deploy_pre_files(&logs, &ver, &files, &cfg, t, clog_t, log).await?);
        res.push(deploy_all_schemas(&logs, client, &ver, &files, dbc, &cfg, t, clog_t, log).await?);
        res.push(deploy_all_files(&logs, &ver, &files, &cfg, t, clog_t, log).await?);
        let _ = draft.after("after_each", files, &logs, cfg, Some(t), log).await?;
    }
    Ok(res)
}

#[inline]
/// deploy changes by files
async fn deploy_pre_files(
    logs: &LogSchemaType,
    ver: &FileVersion,
    files: &FilesMap,
    cfg: &ConfigMap,
    t: &mut Transaction<'_>,
    clog_t: &mut Option<Transaction<'_>>, // copy_log_client
    log: &Logger,
) -> Result<i32, String> {
    let draft = StatementLog::new(cfg);
    // deploy versions
    Ok(
        if let Some(file) = files.pre.get(ver) {
            let id = draft
                .instance('P', &ver.s, &cfg.get("source_dir"), file)
                .deploy(&logs, cfg, Some(t), clog_t, true, log).await?;
            draft.after("after_each", files, &logs, cfg, Some(t), log).await?;
            id
        } else { 0 }
    )
}

#[inline]
/// deploy changes by files
async fn deploy_all_files(
    logs: &LogSchemaType,
    ver: &FileVersion,
    files: &FilesMap,
    cfg: &ConfigMap,
    t: &mut Transaction<'_>,
    clog_t: &mut Option<Transaction<'_>>, // copy_log_client
    log: &Logger,
) -> Result<i32, String> {
    let mut id = 0;
    let draft = StatementLog::new(cfg);
    // deploy versions
    if let Some(file) = &files.versions.get(ver) {
        id = draft
            .instance('V', &ver.s, &cfg.get("source_dir"), file)
            .deploy(&logs, cfg, Some(t), clog_t, true, log).await?;
    }
    // deploy re-runable
    if let Some(file) = &files.re_runnable.get(ver) {
        id = draft
            .instance('R', &ver.s, &cfg.get("source_dir"), file)
            .deploy(&logs, cfg, Some(t), clog_t, true, log).await?;
    }
    Ok(id)
}

#[inline]
/// deploy schema by definitions - declaratives
async fn deploy_all_schemas(
    logs: &LogSchemaType,
    client: &mut Client,
    ver: &FileVersion,
    files: &FilesMap,
    dbc: &mut InfoSchemaType,
    cfg: &ConfigMap,
    lt: &mut Transaction<'_>,
    clog_t: &mut Option<Transaction<'_>>, // copy_log_client
    log: &Logger,
) -> Result<i32, String> {
    let mut id = 0;
    let mut err = None;
    // let mut client = connect("db_", &cfg, None, log).await?;
    {
        let stl = StatementLog::new(cfg);
        let etl = dbm_etl::Etl::from(cfg)?;
        let table_tr = cfg.get("schema_transaction") == "table" || cfg.is_retry();
        let mut do_persist = false;
        let dry_run = cfg.get_bool("dry_run", false);
        // [sg-tokio] the async signature requires Send + Sync on the callback
        let dry_run_fn: Box<dyn Fn(Vec<String>) -> Result<(), String> + Send + Sync> =
            Box::new(|input| cfg.dry_run(input));
        let dry_run_fn = dry_run_fn.as_ref();
        // for (ver, (file, ysv)) in &files.schemas
        // debug!(log, "looking key={} obj[{:?}] in keys [{:?}]", ver, ver, files.schemas.keys());
        if let Some((file, ysv)) = &files.schemas.get(ver) {
            // debug!(log, "deploying Schema {} [{}]", file, ysv.len());
            match client.transaction().await {
                Ok(mut t) => {
                    // [sg-migrate] A schema file migrates as schema_guard_tokio::migrate
                    // migrates one: every schema's tables (Schema::deploy_all_tables), then
                    // their foreign keys (Schema::deploy_all_fk), templates skipped - over
                    // this file's schema_guard parse (dbm_files::parse_schema_file) and the
                    // info schema sge carries from version to version. schema_guard has no
                    // hook for a COMMIT after each changed table (schema_transaction=table,
                    // retry, or a table's own `transaction: table|retry`), so for those sge
                    // steps through the tables itself: the same Table::deploy and deploy_fk,
                    // the same template skip.
                    // [sg-tokio] `deploy` no longer takes is_retry - retry is now a
                    // MigrationOptions concern inside sg, not a per-call flag
                    let opt = migration_options(cfg);
                    let dry = if dry_run { Some(dry_run_fn) } else { None };
                    let per_table = table_tr
                        || ysv.list.iter().any(|ys| ys.tables.list.iter().any(|yt| yt.is_table_transaction()));
                    if !per_table {
                        for ys in &ysv.list {
                            match ys.deploy_all_tables(dbc, &mut t, dry, &opt).await {
                                Ok(n) => {
                                    debug!(log, "deployed {} table(s) of {} from {} dry_run?{}", n, ys.schema_name, file, dry_run);
                                    do_persist = do_persist || n > 0;
                                }
                                Err(e) => {
                                    do_persist = true;
                                    err = Some(e);
                                    break;
                                }
                            }
                        }
                        if err.is_none() {
                            for ys in &ysv.list {
                                match ys.deploy_all_fk(ysv, dbc, &mut t, cfg.is_retry(), dry).await {
                                    Ok(n) => do_persist = do_persist || n > 0,
                                    Err(e) => {
                                        err = Some(e);
                                        break;
                                    }
                                }
                            }
                        }
                    } else {
                        // a failed or uncommitted table stops the file: no table after it is
                        // deployed, or committed, on top of the error
                        'tables: for ys in &ysv.list {
                            for yt in &ys.tables.list {
                                if yt.is_template() {
                                    continue;
                                }
                                match yt.deploy(dbc, &mut t, &ys.schema_name, &ys.file, dry, &opt).await {
                                    Ok(ok) => {
                                        debug!(log, "deployed {}.{}: [{}] from {} dry_run?{}", ys.schema_name, yt.table_name, if ok {"ok"}else{"NOT OK"}, file, dry_run);
                                        if ok {
                                            do_persist = true;
                                            if yt.is_table_transaction() || table_tr {
                                                if let Err(ec) = t.batch_execute("COMMIT; BEGIN").await {
                                                    err = Some(format!(
                                                        "committing change on {}.{} with {}",
                                                        ys.schema_name, yt.table_name, ec
                                                    ));
                                                    break 'tables;
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        do_persist = true;
                                        err = Some(e);
                                        break 'tables;
                                    }
                                }
                            }
                        }
                        if err.is_none() {
                            'fks: for ys in &ysv.list {
                                for yt in &ys.tables.list {
                                    if yt.is_template() {
                                        continue;
                                    }
                                    match yt.deploy_fk(ysv, dbc, &mut t, &ys.schema_name, cfg.is_retry(), &ys.file, dry).await {
                                        Ok(ok) => {
                                            if ok {
                                                do_persist = true;
                                                if yt.is_table_transaction() || table_tr {
                                                    if let Err(ec) = t.batch_execute("COMMIT; BEGIN").await {
                                                        err = Some(format!(
                                                            "committing change on {}.{} with {}",
                                                            ys.schema_name, yt.table_name, ec
                                                        ));
                                                        break 'fks;
                                                    }
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            err = Some(e);
                                            break 'fks;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if err.is_none() {
                        match etl.deploy_all(ysv, logs, dbc, &mut t, lt, clog_t, &stl, log).await {
                            Ok(p) => do_persist = do_persist || p,
                            Err(e) => {
                                do_persist = true;
                                err = Some(e);
                            }
                        }
                    }

                    if !do_persist {
                        error!(log, "rollback");
                        let _ = t.rollback().await; // done with transaction anyway
                    } else {
                        match &err {
                            // err means was persist
                            Some(e) => {
                                error!(log, "rollback: {}", e);
                                let _ = t.rollback().await;
                            }
                            None => {
                                info!(log, "commit: {}", do_persist);
                                if do_persist {
                                    if let Err(e) = t.commit().await {
                                        error!(log, "commit: {}", e);
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
                    .persist(err.clone(), Some(lt), clog_t).await {
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
async fn connect(
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
            crate::dbm_cfg::url_value(&cfg.get_def(
                format!("create{}user", prefix).as_str(),
                cfg.get(format!("{}user", prefix).as_str())
            ))
        );

        let mut log_url = url.clone();
        match cfg.get_opt(format!("create{}pwd", prefix).as_str()) {
            None => {
                cfg.get_pow(&mut url, prefix, "password", "&");
                cfg.get_pow_scr(&mut log_url, prefix, "password", "&");
            }
            Some(pwd) => {
                let _ = write!(url, "&password={}", crate::dbm_cfg::url_value(&pwd));
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
    let db = format!("postgresql:///{}?{}", crate::dbm_cfg::url_value(&dbname), url);
    debug!(log, "{}: connecting: {} {}", module, log_url, dbname);
    match if cfg.is_ssl() {
        #[cfg(not(feature = "tls"))]
        return Err(format!("No openssl on this build yet to connect to: {}", db));
        #[cfg(feature = "tls")]
        spawn_connect_tls(db.as_str(), match make_tls_connector(&cfg.get(format!("{}sslrootcert", prefix).as_str())) {
            Ok(c) => c,
            Err(e) => {
                return Err(format!("create TLS(SSL) connection: {}", e));
            }
        }).await
    } else {
        spawn_connect(db.as_str()).await
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
                            // Boxed: connect() and create_connect() are mutually
                            // recursive, and an async fn that awaits itself has a
                            // state machine of infinite size. The sync version got
                            // away with it because a stack frame is not a type.
                            return Box::pin(create_connect(
                                dbname.as_str(),
                                prefix,
                                cfg,
                                module,
                                log,
                            )).await;
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

/// a password as the logs show it: never - a debug build showed it in clear
#[inline]
fn screen(_pwd: &String) -> &str {
    "***"
}

/// [quote] An identifier as PostgreSQL reads it: as written when it is a plain
/// lower-case name - what it always was here - double-quoted otherwise, so a name
/// with a capital, a space or a quote is that name, and not SQL
pub(crate) fn quote_ident(name: &str) -> String {
    let plain = name.chars().next().map_or(false, |c| c.is_ascii_lowercase() || c == '_')
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '$');
    if plain { name.to_string() } else { format!("\"{}\"", name.replace('"', "\"\"")) }
}

/// [quote] A string literal: single-quoted, its quotes doubled
pub(crate) fn quote_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

#[inline(never)]
async fn create_connect(
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
    let mut client = connect(prefix, cfg, Some(true), module, log).await?;
    let owner = match cfg.get_opt(format!("{}user", prefix).as_str()) {
        None => "".to_string(),
        Some(user) => {
            debug!(log, "testing for user: {}", user);
            create_role(&mut client, &user, prefix, cfg, log).await?;
            format!(" OWNER {}", quote_ident(&user))
        }
    };
    match connect(prefix, cfg, Some(false), module, log).await {
        Ok(_) => { /* [sg-tokio] the connection closes when the client drops */ }
        Err(e) => {
            debug!(log, "{} testing connection: {}", dbname, e);
            info!(log, "NO DB name [{}] found, trying to create...", dbname);
            let _ = client
                .batch_execute(format!("CREATE DATABASE {} {}", quote_ident(dbname), owner).as_str()).await
                .map_err(|e| format!("Creating DB [{}] by postgres: {} ", dbname, e))?;
        }
    }
    info!(log, "DB/user [{}] created, reconnecting", dbname);
    connect(prefix, cfg, Some(false), module, log).await
}

async fn create_role(
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
        ).await
        .map_err(|e| format!("loading role [{}] {}", user, e))?;
    let users: i64 = row.get(0);
    if users == 0 {
        info!(log, "NO user {} found, trying to create ...", user);
        let pwd = match cfg.get_opt(format!("{}password", prefix).as_str()) {
            None => "".to_string(),
            Some(pwd) => format!(" PASSWORD {}", quote_literal(&pwd)),
        };
        let _ = db
            .batch_execute(format!("CREATE ROLE {} WITH LOGIN {}", quote_ident(user), pwd).as_str()).await
            .map_err(|e| format!("Creating user role: {} error: {}", user, e))?;
        info!(log, "Created user role: {}", user);
    }
    Ok(())
}

#[inline]
#[cfg(feature = "tls")]
fn make_tls_connector(cert: &String) -> Result<postgres_native_tls::MakeTlsConnector, String> {
    let cert = fs::read(cert).map_err(|e| format!("reading {} Error: {}", cert, e))?;
    let cert = native_tls::Certificate::from_pem(&cert).map_err(|e| e.to_string())?;
    let connector = native_tls::TlsConnector::builder()
        .add_root_certificate(cert)
        .build().map_err(|e| e.to_string())?;
    Ok( postgres_native_tls::MakeTlsConnector::new(connector))
}

/* openssl
fn make_tls_connector(cert: &String) -> Result<MakeTlsConnector, ErrorStack> {
    let mut builder = SslConnector::builder(SslMethod::tls())?;
    builder.set_ca_file(cert)?;
    Ok(MakeTlsConnector::new(builder.build()))
} */

#[inline]
pub async fn load(dump: bool, cfg: ConfigMap, log: &Logger) -> Result<(), String> {
    let mut log_client = connect("logdb_", &cfg, None, "loading logs",log).await?;
    let mut db_client = connect("db_", &cfg, None, "loading data",log).await?;
    let mut db = db_client.transaction().await.map_err(|e| e.to_string())?;
    let etl = dbm_etl::Etl::from(&cfg)?;
    let result = if dump {
        LoadResult::load(&etl, &mut db, log).await?.format(etl.yaml_format)
    } else {
        StatementLog::load_formatted(&cfg, &mut log_client).await
    };
    drop(db);
    etl.dump_loaded(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_passwords_are_quoted_not_pasted() {
        // a plain lower-case name as it always was; anything else its own name
        assert_eq!(quote_ident("app_user1"), "app_user1");
        assert_eq!(quote_ident("App"), "\"App\"");
        assert_eq!(quote_ident("a b"), "\"a b\"");
        assert_eq!(quote_ident("x\"; drop role y; --"), "\"x\"\"; drop role y; --\"");
        assert_eq!(quote_ident(""), "\"\"");
        assert_eq!(quote_literal("p'w"), "'p''w'");
        assert_eq!(quote_literal("plain"), "'plain'");
    }

    #[test]
    fn a_connection_url_keeps_a_password_with_url_characters() {
        // built as connect() builds it, read back by tokio-postgres itself
        let cfg = ConfigMap::of(&[("db_user", "app"), ("db_password", "p&s=w%d#?/ x"), ("db_host", "/var/run/postgresql")]);
        let mut url = String::new();
        for name in ["host", "user", "password"].iter() {
            cfg.get_pow(&mut url, "db_", name, "&");
        }
        let db = format!("postgresql:///{}?{}", crate::dbm_cfg::url_value("my db"), url);
        let c: tokio_postgres::Config = db.parse().expect(db.as_str());
        assert_eq!(c.get_password(), Some("p&s=w%d#?/ x".as_bytes()), "{}", db);
        assert_eq!(c.get_user(), Some("app"), "{}", db);
        assert_eq!(c.get_dbname(), Some("my db"), "{}", db);
    }

    #[test]
    fn migration_options_follow_the_clusters_sg_migration() {
        let o = migration_options(&ConfigMap::of(&[]));
        assert!(!o.with_size_cut && !o.with_index_drop && !o.without_failfast && !o.with_ddl_retry);
        let o = migration_options(&ConfigMap::of(&[("sg_migration", "with_size_cut, With_Index_Drop")]));
        assert!(o.with_size_cut && o.with_index_drop);
        assert!(!o.with_trigger_drop && !o.exclude_triggers && !o.with_revoke && !o.without_failfast);
        // schema_transaction=retry keeps meaning ddl retry
        assert!(migration_options(&ConfigMap::of(&[("schema_transaction", "retry")])).with_ddl_retry);
    }

    #[test]
    fn statementlog_deser_test() {
        assert!(STATEMENT_LOG_TBL.table_name.len() > 1);
    }


    #[test]
    fn config_log_deser_test() {
        assert!(crate::CONFIG_APP.len() > 15);
    }
}
