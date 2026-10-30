//! sge (Schema Guard Enterprise): no unsafe code - the compiler holds it to that
#![forbid(unsafe_code)]

#[macro_use]
extern crate slog;
use lazy_static::lazy_static;

extern crate semver;
extern crate yaml_rust2;

// [i18n] locales/*.yml, compiled in; a language without a message is English
rust_i18n::i18n!("locales", fallback = "en");

// first: tr! is declared there, for the modules below
#[macro_use]
pub mod i18n;
/// [pg] Postgres's errors, in full and in one place
mod dbm_pg;
pub mod dbm_cfg;
mod dbm_log;
mod dbm_files;
/// [git] git repository as a script source - see `source_git`
mod dbm_git;
/// [zip] zip archive as a script source, unpacked for the run - see `source_zip`
mod dbm_zip;
#[cfg(feature = "superset")]
pub(crate) mod dbm_superset;
/// [yt] rolling migration across a YT Cluster, node by node
pub mod dbm_yt;
/// [plan] what migrate would change, per table, for a review - see `plan`
mod dbm_digest;
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
use schema_guard_tokio::changes::{Changes, DryRun};
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
pub async fn migrate(cfg: ConfigMap, prepare_only: bool, log: &Logger) -> Result<(), String> {
    migrate_run(cfg, prepare_only, log).await
}

/// [plan] What migrate would change, as Markdown on standard output: migrate's dry
/// run - the database read, selects alone; '--createdb' not followed. Its own scope:
/// the digest is this run's
pub async fn plan(cfg: ConfigMap, log: &Logger) -> Result<(), String> {
    print!("{}", plan_md(cfg, log).await?);
    Ok(())
}

async fn plan_md(mut cfg: ConfigMap, log: &Logger) -> Result<String, String> {
    cfg.insert(&"dry_run".to_string(), "yes");
    cfg.insert(&"createdb".to_string(), "false");
    dbm_digest::scope(async {
        dbm_digest::start(&cfg);
        let result = migrate_run(cfg.clone(), false, log).await;
        let collected = dbm_digest::take();
        result?;
        // [plan] the database asked, read only, what the run's steps hinge on
        let db = connect("db_", &cfg, None, "plan", log).await?;
        Ok(match collected {
            Some(c) => dbm_digest::finish(c, Some(&db)).await,
            None => String::new(),
        })
    }).await
}

async fn migrate_run(mut cfg: ConfigMap, prepare_only: bool, log: &Logger) -> Result<(), String> {
    trace!(log, "{}", tr!("lib.trace_migrate", prepare = prepare_only, logdb = cfg.get("logdb"),
        is_logdb = cfg.is_logdb(), is_primary = cfg.is_primary()));
    // [zip] an archive is unpacked first and source_dir pointed into it; the guard
    // removes the directory when this function returns - completed or failed
    let _zip = dbm_zip::materialize(&mut cfg, log)?;
    // [git] a repository is checked out first and source_dir pointed into it, as above
    dbm_git::point(&mut cfg, log)?;
    let mut client = connect(if cfg.is_primary() {"logdb_"} else {"db_"}, &cfg, None, "logdata", log).await?;
    let mut copy_log = if !cfg.is_primary() && cfg.is_logdb() {
        trace!(log, "{}", tr!("lib.trace_copylog"));
        Some(connect("logdb_", &cfg, None, "copylog", log).await?)
    } else { None};
    // [i18n] the logger carries the language, which schema_guard takes from it
    schema_guard_tokio::set_logger(log.new(o!("lang" => i18n::lang())));
    // [yt] the cluster's SchemaGuard options, for every node the roll reaches -
    // node_cfg clones this map
    if dbm_yt::enabled(&cfg) {
        if let Some(sg) = dbm_yt::sg_migration(&mut client, &cfg).await {
            debug!(log, "{}", tr!("lib.yt_options", options = sg));
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
    let mut dbc = load_info_schema(log_db_name.as_str(), &mut log_client.transaction().await
        .map_err(|e| tr!("lib.begin", err = dbm_pg::pg(&e)))?).await?;
    let files = load_all(cfg, log)?;
    let mut t = log_client.transaction().await.unwrap(); // begin self install if not exists
    let mut err = baseline_deploy(&mut dbc, &mut t, &schema, cfg, "primary", log).await;

    let mut ct = match copy_log_client {
        None => {None}
        Some(lc) => {
            if err.is_none() {
                // debug!(log, "begin self install if not exists on log copy instance");
                // TODO reuse loaded?
                let mut t = lc.transaction().await.map_err(|e| tr!("lib.begin", err = dbm_pg::pg(&e)))?;
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
        warn!(log, "{}", tr!("lib.rollback_self", err = e));
        if let Err(e) = t.rollback().await {
            warn!(log, "{}", tr!("lib.rollback_self_failed", err = dbm_pg::pg(&e)));
        }
        return Err(e);
    };
    let res = if prepare_only {
        Ok(vec![])
    } else {
        // select table for update -- row level lock, revert changes
        let logs = { StatementLog::load_all(&mut t, cfg.is_dry_run(), cfg).await? };

        let db_name = cfg.get("db_name");
        let mut dbc = if log_db_name != db_name {
            let mut client = connect("db_", &cfg, None, "loading info schema",log).await?;
            let dbc = load_info_schema(db_name.as_str(), &mut client.transaction().await
                .map_err(|e| tr!("lib.begin", err = dbm_pg::pg(&e)))?).await?;
            debug!(log, "{}", tr!("lib.info_schema_done"));
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
                    debug!(log, "{}", tr!("lib.all_done"));
                    Ok(())
                },
                Err(e) => Err(e.to_string()),
            }
        },
        Err(e) => {
            error!(log, "{}", tr!("lib.commit_history_log", err = dbm_pg::pg(&e)));
            match &res {
                Ok(_) => Err(tr!("lib.commit_history", err = dbm_pg::pg(&e))),
                Err(ee) => Err(tr!("lib.commit_history_also", err = ee, commit_err = dbm_pg::pg(&e))),
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
    debug!(log, "{}", tr!("lib.self_deploy_start", what = BASELINE, cause = cause));
    let mut baseline = crate::STATEMENT_LOG_TBL.clone();
    baseline.table_name = cfg.history_table_name();
    // [sg-tokio] deploy is async, dropped is_retry, gained MigrationOptions,
    // and the callback must be Send + Sync
    // [digest] its schema.table and changes, beside the SQL
    let dr: Box<DryRun<'_>> = Box::new(|input: Vec<String>, table: Option<&str>, changes: Option<&Changes>| {
        dbm_digest::changes(table, changes);
        cfg.dry_run(input)
    });
    dbm_digest::source('B', "", BASELINE, None);
    match baseline.deploy_plan_pg(dbc, t, &schema, BASELINE,
      if cfg.is_dry_run() { Some(dr.as_ref()) } else {None},
      &migration_options(cfg)
    ).await {
        Ok(exec) => {
            if exec {
                debug!(log, "{}", tr!("lib.self_deploy_done", what = BASELINE));
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
        Err(e) => Some(dbm_pg::deploy(e)),
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
        info!(log, "{}", tr!("lib.deploying_version", version = ver));
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
        let dry_run = cfg.is_dry_run();
        // [sg-tokio] the async signature requires Send + Sync on the callback
        // [digest] each table's schema.table and changes, beside the SQL
        let dry_run_fn: Box<DryRun<'_>> = Box::new(|input: Vec<String>, table: Option<&str>, changes: Option<&Changes>| {
            dbm_digest::changes(table, changes);
            cfg.dry_run(input)
        });
        let dry_run_fn = dry_run_fn.as_ref();
        // for (ver, (file, ysv)) in &files.schemas
        // debug!(log, "looking key={} obj[{:?}] in keys [{:?}]", ver, ver, files.schemas.keys());
        if let Some((file, ysv)) = &files.schemas.get(ver) {
            dbm_digest::source('S', &ver.s, file, Some(&dbm_digest::path(&cfg.get("source_dir"), file)));
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
                            match ys.deploy_all_tables_plan_pg(dbc, &mut t, dry, &opt).await {
                                Ok(n) => {
                                    debug!(log, "{}", tr!("lib.deployed_tables", count = n, schema = ys.schema_name, file = file, dry_run = dry_run));
                                    do_persist = do_persist || n > 0;
                                }
                                Err(e) => {
                                    do_persist = true;
                                    err = Some(dbm_pg::deploy(e));
                                    break;
                                }
                            }
                        }
                        if err.is_none() {
                            for ys in &ysv.list {
                                match ys.deploy_all_fk_plan_pg(ysv, dbc, &mut t, cfg.is_retry(), dry).await {
                                    Ok(n) => do_persist = do_persist || n > 0,
                                    Err(e) => {
                                        err = Some(dbm_pg::deploy(e));
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
                                match yt.deploy_plan_pg(dbc, &mut t, &ys.schema_name, &ys.file, dry, &opt).await {
                                    Ok(ok) => {
                                        debug!(log, "{}", tr!(if ok { "lib.deployed_table_ok" } else { "lib.deployed_table_failed" },
                                            table = format!("{}.{}", ys.schema_name, yt.table_name), file = file, dry_run = dry_run));
                                        if ok {
                                            do_persist = true;
                                            if yt.is_table_transaction() || table_tr {
                                                if let Err(ec) = t.batch_execute("COMMIT; BEGIN").await {
                                                    err = Some(tr!("lib.commit_table", table = format!("{}.{}", ys.schema_name, yt.table_name), err = dbm_pg::pg(&ec)));
                                                    break 'tables;
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        do_persist = true;
                                        err = Some(dbm_pg::deploy(e));
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
                                    match yt.deploy_fk_plan_pg(ysv, dbc, &mut t, &ys.schema_name, cfg.is_retry(), &ys.file, dry).await {
                                        Ok(ok) => {
                                            if ok {
                                                do_persist = true;
                                                if yt.is_table_transaction() || table_tr {
                                                    if let Err(ec) = t.batch_execute("COMMIT; BEGIN").await {
                                                        err = Some(tr!("lib.commit_table", table = format!("{}.{}", ys.schema_name, yt.table_name), err = dbm_pg::pg(&ec)));
                                                        break 'fks;
                                                    }
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            err = Some(dbm_pg::deploy(e));
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
                        error!(log, "{}", tr!("lib.rollback"));
                        let _ = t.rollback().await; // done with transaction anyway
                    } else {
                        match &err {
                            // err means was persist
                            Some(e) => {
                                error!(log, "{}", tr!("lib.rollback_err", err = e));
                                let _ = t.rollback().await;
                            }
                            None => {
                                info!(log, "{}", tr!("lib.commit_log", persist = do_persist));
                                if do_persist {
                                    if let Err(e) = t.commit().await {
                                        error!(log, "{}", tr!("lib.commit_failed_log", err = dbm_pg::pg(&e)));
                                        err = Some(tr!("lib.commit_file", file = file, err = dbm_pg::pg(&e)))
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => err = Some(tr!("lib.transaction", file = file, err = dbm_pg::pg(&e))),
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
        debug!(log, "{}", tr!("lib.create_db_connect", module = module, url = log_url));
        ("postgres".to_string(), log_url)
    } else {
        cfg.get_pow(&mut url, prefix, "user", "&");
        let mut log_url = url.clone();
        cfg.get_pow(&mut url, prefix, "password", "&");
        cfg.get_pow_scr(&mut log_url, prefix, "password", "&");
        match cfg.get_opt(format!("{}name", prefix).as_str()) {
            None => {
                return Err(tr!("lib.no_db_name", prefix = prefix));
            }
            Some(db) => (db, log_url),
        }
    };
    let db = format!("postgresql:///{}?{}", crate::dbm_cfg::url_value(&dbname), url);
    debug!(log, "{}", tr!("lib.connecting", module = module, url = log_url, db = dbname));
    match if cfg.is_ssl() {
        #[cfg(not(feature = "tls"))]
        return Err(tr!("lib.no_tls", db = format!("postgresql:///{}?{}", crate::dbm_cfg::url_value(&dbname), log_url)));   // its password as the logs show it
        #[cfg(feature = "tls")]
        spawn_connect_tls(db.as_str(), match make_tls_connector(&cfg.get(format!("{}sslrootcert", prefix).as_str())) {
            Ok(c) => c,
            Err(e) => {
                return Err(tr!("lib.tls", err = e));
            }
        }).await
    } else {
        spawn_connect(db.as_str()).await
    } {
        Ok(client) => {
            schema_guard_tokio::set_lc_messages(&client, &i18n::lang()).await;
            Ok(client)
        }
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
                            error!(log, "{}", tr!("lib.no_db", db = dbname));
                        }
                    } else {
                        debug!(log, "{}", tr!("lib.connection_code", module = module, code = format!("{:?}", code)));
                    }
                }
            }
            Err(tr!("lib.connect", url = log_url, db = dbname, err = dbm_pg::pg(&err)))
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
    info!(log, "{}", tr!("lib.try_superuser", db = dbname));
    let mut client = connect(prefix, cfg, Some(true), module, log).await?;
    let owner = match cfg.get_opt(format!("{}user", prefix).as_str()) {
        None => "".to_string(),
        Some(user) => {
            debug!(log, "{}", tr!("lib.testing_user", user = user));
            create_role(&mut client, &user, prefix, cfg, log).await?;
            format!(" OWNER {}", quote_ident(&user))
        }
    };
    match connect(prefix, cfg, Some(false), module, log).await {
        Ok(_) => { /* [sg-tokio] the connection closes when the client drops */ }
        Err(e) => {
            debug!(log, "{}", tr!("lib.testing_connection", db = dbname, err = e));
            info!(log, "{}", tr!("lib.creating_db", db = dbname));
            let _ = client
                .batch_execute(format!("CREATE DATABASE {} {}", quote_ident(dbname), owner).as_str()).await
                .map_err(|e| tr!("lib.create_db", db = dbname, err = dbm_pg::pg(&e)))?;
        }
    }
    info!(log, "{}", tr!("lib.db_created", db = dbname));
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
        .map_err(|e| tr!("lib.load_role", user = user, err = dbm_pg::pg(&e)))?;
    let users: i64 = row.get(0);
    if users == 0 {
        info!(log, "{}", tr!("lib.creating_role", user = user));
        let pwd = match cfg.get_opt(format!("{}password", prefix).as_str()) {
            None => "".to_string(),
            Some(pwd) => format!(" PASSWORD {}", quote_literal(&pwd)),
        };
        let _ = db
            .batch_execute(format!("CREATE ROLE {} WITH LOGIN {}", quote_ident(user), pwd).as_str()).await
            .map_err(|e| tr!("lib.create_role", user = user, err = dbm_pg::pg(&e)))?;
        info!(log, "{}", tr!("lib.role_created", user = user));
    }
    Ok(())
}

#[inline]
#[cfg(feature = "tls")]
fn make_tls_connector(cert: &String) -> Result<postgres_native_tls::MakeTlsConnector, String> {
    let cert = std::fs::read(cert).map_err(|e| tr!("lib.read_cert", file = cert, err = e))?;
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
    let mut db = db_client.transaction().await.map_err(|e| tr!("lib.begin", err = dbm_pg::pg(&e)))?;
    let etl = dbm_etl::Etl::from(&cfg)?;
    // [superset] the snapshot as Apache Superset's import bundle: a ZIP, not text
    if dump && etl.superset {
        #[cfg(not(feature = "superset"))]
        return Err(tr!("etl.superset_off"));
        #[cfg(feature = "superset")]
        {
            // the role this connection is: the URI's user, when db_user named none
            let user: String = db.query_one("select current_user::text", &[]).await
                .map_err(|e| tr!("etl.superset_user", err = dbm_pg::pg(&e)))?.get(0);
            // the cheap checks first - a bad --superset_uuid, a terminal for a ZIP - not
            // after the whole schema was read (review of 2026-10-26)
            let target = dbm_superset::database(&cfg, &user)?;
            etl.superset_writable()?;
            // the schema alone: the bundle uses no extracted data
            let loaded = LoadResult::load_schema(&etl, &mut db).await?;
            drop(db);
            return etl.dump_superset(&loaded, &target);
        }
    }
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

    fn log() -> Logger {
        Logger::root(slog::Discard, o!())
    }

    /// [plan] a throwaway cluster of the newest PostgreSQL installed - PG_BIN, else
    /// /usr/lib/postgresql/<ver>/bin - on its unix socket alone; stopped and removed when
    /// dropped. None without one: the test says so and passes
    pub(crate) struct Pg {
        bin: std::path::PathBuf,
        pub(crate) dir: std::path::PathBuf,
        pub(crate) port: u16,
    }

    impl Pg {
        pub(crate) fn start(name: &str) -> Option<Pg> {
            let bin = match std::env::var("PG_BIN") {
                Ok(b) => std::path::PathBuf::from(b),
                Err(_) => {
                    let mut v: Vec<(u32, std::path::PathBuf)> = std::fs::read_dir("/usr/lib/postgresql").ok()?
                        .filter_map(|e| e.ok())
                        .filter_map(|e| Some((e.file_name().to_str()?.parse().ok()?, e.path().join("bin"))))
                        .filter(|(_, b): &(u32, std::path::PathBuf)| b.join("initdb").exists())
                        .collect();
                    v.sort();
                    match v.pop() {
                        Some((_, b)) => b,
                        None => {
                            eprintln!("no PostgreSQL installed (PG_BIN, /usr/lib/postgresql): skipped");
                            return None;
                        }
                    }
                }
            };
            let dir = std::env::temp_dir().join(format!("sge-{}-{}", name, std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let port = 40000 + (std::process::id() % 10000) as u16;
            let run = |c: &mut std::process::Command| c.output().unwrap().status.success();
            assert!(run(std::process::Command::new(bin.join("initdb")).args(["-A", "trust", "-U", "postgres", "-E", "UTF8", "--no-locale", "-D"])
                .arg(dir.join("data"))), "initdb");
            let pg = Pg { bin, dir, port };
            assert!(run(std::process::Command::new(pg.bin.join("pg_ctl")).args(["-w", "-l"]).arg(pg.dir.join("log")).arg("-D").arg(pg.dir.join("data"))
                .arg("-o").arg(format!("-c listen_addresses='' -k {} -p {}", pg.dir.display(), pg.port)).arg("start")),
                "pg_ctl start: {}", std::fs::read_to_string(pg.dir.join("log")).unwrap_or_default());
            Some(pg)
        }
    }

    impl Drop for Pg {
        fn drop(&mut self) {
            let _ = std::process::Command::new(self.bin.join("pg_ctl")).args(["-w", "-m", "immediate", "-D"]).arg(self.dir.join("data"))
                .arg("stop").output();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// [plan] sge plan against a real server: sge's history table, a schema file's table
    /// and its data file's rows - with the schema file's ticket - a script's insert by its
    /// own ticket, an after script; and the database as it was, nothing written
    #[tokio::test]
    async fn plan_says_what_migrate_would_change() {
        let Some(pg) = Pg::start("plan") else { return };
        let src = pg.dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("S1__schema.yaml"), r#"# PROJ-1
database:
  - schema:
    schemaName: app
    tables:
      - table:
          tableName: t
          data_file: D1__t.csv
          columns:
            - column: {name: id, type: serial, constraint: {primaryKey: true, nullable: false}}
            - column: {name: name, type: varchar(50)}
"#).unwrap();
        std::fs::write(src.join("D1__t.csv"), "id,name\n1,a\n2,b\n").unwrap();
        std::fs::write(src.join("V1_1__v.sql"), "-- PROJ-2\nINSERT INTO app.t (name) VALUES ('c');\n").unwrap();
        std::fs::write(src.join("after.sql"), "UPDATE app.t SET name = name;\n").unwrap();
        let host = pg.dir.to_string_lossy().to_string();
        let mut cfg = ConfigMap::load("").unwrap();
        for (k, v) in [("db_host", host.as_str()), ("db_port", &pg.port.to_string()), ("db_user", "postgres"), ("db_name", "postgres"),
            ("source_dir", &src.to_string_lossy()), ("jira_prefix", "PROJ"), ("createdb", "yes")] {
            cfg.insert(&k.to_string(), v);
        }
        let md = plan_md(cfg, &log()).await.unwrap();
        assert!(md.starts_with(&format!("## {}\n", tr!("digest.title", target = format!("postgres@{}", host)))), "{}", md);
        assert!(md.contains(concat!("\n### PROJ-1\n\n- **app.t**\n",
            "  - `S1__schema.yaml`: ddl create (id, name)\n",
            "  - `D1__t.csv`: ddl sequence setval | dml insert 2\n")), "{}", md);
        assert!(md.contains("\n### PROJ-2\n\n- **app.t**\n  - `V1_1__v.sql`: dml insert 1\n"), "{}", md);
        // app.t is the plan's own: its rows are not counted
        assert!(md.contains("  - `after.sql`: dml update 1 statement\n"), "{}", md);
        assert!(md.contains(&format!("- **public.sge_history**\n  - {}: ddl create (", tr!("digest.history"))), "{}", md);

        // a plan reads: no history table, no schema written
        let (c, conn) = tokio_postgres::connect(&format!("host={} port={} user=postgres dbname=postgres", host, pg.port), NoTls).await.unwrap();
        tokio::spawn(conn);
        let n: i64 = c.query_one("select count(*) from pg_tables where tablename = 'sge_history' or schemaname = 'app'", &[]).await.unwrap().get(0);
        assert_eq!(n, 0);
    }

    /// [plan] the database asked, read only: IF [NOT] EXISTS that does not hold is no
    /// change; the rows an UPDATE's and a DELETE's WHERE select, an INSERT's whose key is
    /// there or not, a DO's IF's branch, a data file's rows by key - with no key, by every
    /// column; its clean's deletes; a TRUNCATE's, the rows its table has - counted; on a
    /// table the plan creates, and a count the database refuses - a nextval(), after a
    /// set_config() that turns read only off - its statements; COPY and INSERT ... SELECT
    /// not read. An IF whose condition is NULL takes its ELSE, one with a quote and a
    /// comment is read. Each kind of object asked for, by its schema
    /// and under a search_path. Nothing written, the sequence not taken from; a role that
    /// reads alone makes the same plan
    #[tokio::test]
    async fn plan_asks_the_database() {
        let Some(pg) = Pg::start("ask") else { return };
        let host = pg.dir.to_string_lossy().to_string();
        let (c, conn) = tokio_postgres::connect(&format!("host={} port={} user=postgres dbname=postgres", host, pg.port), NoTls).await.unwrap();
        tokio::spawn(conn);
        c.batch_execute("CREATE TABLE public.t (id int primary key, name text);\n\
            INSERT INTO public.t VALUES (1, 'a'), (2, 'b'), (3, 'c');\n\
            CREATE TABLE public.e (id int primary key, v int);\n\
            INSERT INTO public.e VALUES (1, 10), (5, 50);\n\
            CREATE TABLE public.k (a int, b text);\n\
            INSERT INTO public.k VALUES (1, 'x');\n\
            CREATE TABLE public.a (x int);\n\
            INSERT INTO public.a VALUES (1);\n\
            CREATE TABLE public.o (id int);\n\
            ALTER TABLE public.o ADD CONSTRAINT o_chk CHECK (id > 0);\n\
            CREATE POLICY pol ON public.o USING (true);\n\
            CREATE FUNCTION public.tf() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN RETURN NEW; END$$;\n\
            CREATE TRIGGER trg BEFORE INSERT ON public.o FOR EACH ROW EXECUTE FUNCTION public.tf();\n\
            CREATE FUNCTION public.f() RETURNS int LANGUAGE sql AS 'SELECT 1';\n\
            CREATE TYPE public.ty AS (a int);\n\
            CREATE SCHEMA shop;\n\
            CREATE SEQUENCE public.s;").await.unwrap();
        let src = pg.dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("S1__schema.yaml"), r#"database:
  - schema:
    schemaName: public
    tables:
      - table:
          tableName: e
          data_file: D1__e.csv
          columns:
            - column: {name: id, type: int, constraint: {primaryKey: true, nullable: false}}
            - column: {name: v, type: int}
      - table:
          tableName: k
          data_file: D1__k.csv
          columns:
            - column: {name: a, type: int}
            - column: {name: b, type: text}
"#).unwrap();
        std::fs::write(src.join("D1__e.csv"), "id,v\n1,10\n2,20\n3,30\n").unwrap();
        std::fs::write(src.join("D1__k.csv"), "a,b\n1,x\n2,y\n2,y\n").unwrap();
        std::fs::write(src.join("V1_1__smart.sql"), "-- PROJ-7\n\
            CREATE TABLE IF NOT EXISTS public.t (id int);\n\
            DROP TABLE IF EXISTS public.gone;\n\
            CREATE TABLE IF NOT EXISTS public.n (id int);\n\
            ALTER TABLE public.t ADD COLUMN IF NOT EXISTS name text;\n\
            ALTER TABLE public.t ADD COLUMN IF NOT EXISTS email text;\n\
            CREATE INDEX IF NOT EXISTS t_pkey ON public.t (id);\n\
            UPDATE public.t SET name = 'x' WHERE id >= 2;\n\
            DELETE FROM public.t WHERE id = 99;\n\
            DELETE FROM public.t WHERE id = 1;\n\
            INSERT INTO public.t (id, name) VALUES (3, 'c'), (4, 'd') ON CONFLICT (id) DO NOTHING;\n\
            INSERT INTO public.t (id, name) VALUES (2, 'z') ON CONFLICT (id) DO UPDATE SET name = excluded.name;\n\
            INSERT INTO public.t SELECT * FROM public.t;\n\
            COPY public.t FROM stdin;\n9\tz\n\\.\n\
            DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_class WHERE relname = 't') THEN ALTER TABLE public.t ADD COLUMN never int; END IF; END $$;\n\
            DO $$ BEGIN IF (SELECT count(*) FROM public.t WHERE name <> 'O''Brien') > 0 -- rows there\n THEN UPDATE public.t SET name = 'y' WHERE id = 3; ELSE DELETE FROM public.t; END IF; END $$;\n\
            DO $$ BEGIN IF (SELECT name FROM public.t WHERE id = 99) = 'on' THEN UPDATE public.k SET b = 'n'; ELSE DELETE FROM public.k WHERE a = 1; END IF; END $$;\n\
            UPDATE public.a SET x = 2 WHERE set_config('default_transaction_read_only', 'off', false) = 'off';\n\
            UPDATE public.e SET v = 1 WHERE id = nextval('public.s');\n\
            UPDATE public.n SET id = 1;\n\
            TRUNCATE public.k, public.n;\n\
            ALTER TABLE public.o DROP CONSTRAINT IF EXISTS no_chk;\n\
            ALTER TABLE public.o DROP CONSTRAINT IF EXISTS o_chk;\n\
            DROP TRIGGER IF EXISTS notrg ON public.o;\n\
            DROP TRIGGER IF EXISTS trg ON public.o;\n\
            DROP POLICY IF EXISTS nopol ON public.o;\n\
            DROP POLICY IF EXISTS pol ON public.o;\n\
            CREATE INDEX IF NOT EXISTS o_id ON public.o (id);\n\
            CREATE EXTENSION IF NOT EXISTS plpgsql;\n\
            CREATE SCHEMA IF NOT EXISTS shop;\n\
            CREATE SEQUENCE IF NOT EXISTS public.s;\n\
            CREATE SEQUENCE IF NOT EXISTS s2;\n\
            DROP FUNCTION IF EXISTS public.nof();\n\
            DROP FUNCTION IF EXISTS f();\n\
            DROP TYPE IF EXISTS public.noty;\n\
            DROP TYPE IF EXISTS ty;\n\
            SET search_path TO shop, public;\n\
            ALTER TABLE IF EXISTS o ADD COLUMN IF NOT EXISTS id int;\n\
            ALTER TABLE o ADD COLUMN IF NOT EXISTS z int;\n\
            RESET search_path;\n").unwrap();
        let cfg = |user: &str| {
            let mut cfg = ConfigMap::load("").unwrap();
            for (k, v) in [("db_host", host.as_str()), ("db_port", &pg.port.to_string()), ("db_user", user), ("db_name", "postgres"),
                ("source_dir", &src.to_string_lossy()), ("jira_prefix", "PROJ"), ("etl", "clean"), ("snapshot_data", "public.e")] {
                cfg.insert(&k.to_string(), v);
            }
            cfg
        };
        let md = plan_md(cfg("postgres"), &log()).await.unwrap();
        // a role that reads alone makes the same plan (pg_read_all_data: PostgreSQL 14 on)
        let v: i32 = c.query_one("SELECT current_setting('server_version_num')::int", &[]).await.unwrap().get(0);
        if v >= 140000 {
            c.batch_execute("CREATE ROLE reader LOGIN; GRANT pg_read_all_data TO reader;").await.unwrap();
            assert_eq!(plan_md(cfg("reader"), &log()).await.unwrap(), md);
        }
        assert!(md.contains(concat!("\n### PROJ-7\n\n",
            "- **public.a**\n  - `V1_1__smart.sql`: dml update 1\n",
            "- **public.e**\n  - `V1_1__smart.sql`: dml update 1 statement\n",
            "- **public.k**\n  - `V1_1__smart.sql`: dml delete 1, truncate 1\n",
            "- **public.n**\n  - `V1_1__smart.sql`: ddl create (id) | dml update 1 statement, truncate 1 statement\n",
            "- **public.o**\n  - `V1_1__smart.sql`: ddl alter: drop constraint o_chk; drop trigger trg; drop policy pol; index o_id (id); alter: add z\n",
            "- **public.t**\n  - `V1_1__smart.sql`: ddl alter: add email | dml insert 1, update 4, delete 1\n",
            "- **other**\n  - `V1_1__smart.sql`: ddl sequence public.s2; drop function f; drop type ty\n")), "{}", md);
        // clean: id 5 is not in the file; k: a row met twice is one, 1 x there already
        assert!(md.contains("- **public.e**\n  - `D1__e.csv`: dml insert 2, update 1, delete 1\n"), "{}", md);
        assert!(md.contains("- **public.k**\n  - `D1__k.csv`: dml insert 1\n"), "{}", md);
        assert!(md.contains(&tr!("digest.counts")), "{}", md);
        for not in ["gone", "t_pkey", "never", "add name", "copy", "no_chk", "notrg", "nopol", "plpgsql", "schema shop", "nof", "noty",
            "add id", "shop.o"] {
            assert!(!md.contains(not), "{} in {}", not, md);
        }

        // nothing written, nothing taken
        for (t, rows) in [("public.t", 3), ("public.e", 2), ("public.k", 1)] {
            let n: i64 = c.query_one(format!("select count(*) from {}", t).as_str(), &[]).await.unwrap().get(0);
            assert_eq!(n, rows, "{}", t);
        }
        let cols: i64 = c.query_one("select count(*) from information_schema.columns where table_name = 't'", &[]).await.unwrap().get(0);
        assert_eq!(cols, 2);
        let called: bool = c.query_one("select is_called from public.s", &[]).await.unwrap().get(0);
        assert!(!called, "a plan takes no value from a sequence");
    }

    /// [git] a git source runs from its checkout - source_dir names an empty directory:
    /// the plan reads its script, with its ticket, and migrate runs it, checksummed
    #[tokio::test]
    async fn a_git_source_runs_from_its_checkout() {
        let Some(pg) = Pg::start("git") else { return };
        let origin = pg.dir.join("origin");
        std::fs::create_dir_all(origin.join("sql")).unwrap();
        let body = "-- PROJ-3\nCREATE TABLE public.g (id int);\n";
        std::fs::write(origin.join("sql/V1_1__g.sql"), body).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=sge", "-c", "user.email=sge@example.invalid", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
                .args(args).current_dir(&origin).output().unwrap();
            assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "g"]);
        std::fs::create_dir_all(pg.dir.join("empty")).unwrap();
        let host = pg.dir.to_string_lossy().to_string();
        let cfg = || {
            let mut cfg = ConfigMap::load("").unwrap();
            for (k, v) in [("db_host", host.as_str()), ("db_port", &pg.port.to_string()), ("db_user", "postgres"), ("db_name", "postgres"),
                ("source_dir", &pg.dir.join("empty").to_string_lossy()), ("source_git", &format!("file://{}", origin.display())),
                ("source_git_dir", "sql"), ("source_git_cache", &pg.dir.join("cache").to_string_lossy()),
                ("jira_prefix", "PROJ"), ("createdb", "yes")] {
                cfg.insert(&k.to_string(), v);
            }
            cfg
        };

        let md = plan_md(cfg(), &log()).await.unwrap();
        assert!(md.contains("\n### PROJ-3\n\n- **public.g**\n  - `V1_1__g.sql`: ddl create (id)\n"), "{}", md);
        assert!(!md.contains(&tr!("digest.not_read", err = "")), "{}", md);

        migrate(cfg(), false, &log()).await.unwrap();
        let (c, conn) = tokio_postgres::connect(&format!("host={} port={} user=postgres dbname=postgres", host, pg.port), NoTls).await.unwrap();
        tokio::spawn(conn);
        let n: i64 = c.query_one("select count(*) from pg_tables where tablename = 'g'", &[]).await.unwrap().get(0);
        assert_eq!(n, 1);
        // the checksum of the checkout's file; script_tag is process-wide (dbm_git::REVISION),
        // other tests' too
        let r = c.query_one("select checksum from public.sge_history where source = 'V1_1__g.sql'", &[]).await.unwrap();
        use base64::Engine;
        use md5::Digest;
        assert_eq!(r.get::<_, String>(0), base64::engine::general_purpose::STANDARD.encode(md5::Md5::digest(body.as_bytes())));
    }

    /// [connect] what is refused before any server is asked: no database named, TLS
    /// on a build without it; and a server that is not there, as the superuser too
    #[tokio::test]
    async fn a_connection_is_refused_before_any_server_is_asked() {
        let e = connect("db_", &ConfigMap::of(&[("db_host", "127.0.0.1")]), None, "t", &log()).await.err().unwrap();
        assert!(e.starts_with("SGE-E006 ") && e.contains("db_name"), "{}", e);
        #[cfg(not(feature = "tls"))]
        {
            let c = ConfigMap::of(&[("db_name", "app"), ("db_sslcert", "/c.pem"), ("db_password", "secret")]);
            let e = connect("db_", &c, None, "t", &log()).await.err().unwrap();
            assert!(e.starts_with("SGE-E007 ") && e.contains("password=***") && !e.contains("secret"), "{}", e);
        }
        let nowhere = [("db_host", "127.0.0.1"), ("db_port", "1"), ("db_name", "app"), ("db_user", "app"),
            ("db_password", "secret"), ("createdb", "true")];
        let e = connect("db_", &ConfigMap::of(&nowhere), Some(true), "t", &log()).await.err().unwrap();
        assert!(e.starts_with("SGE-E009 ") && e.contains("password=***") && !e.contains("secret"), "{}", e);
        // logdb_ with no log database of its own: the main one's settings
        let e = connect("logdb_", &ConfigMap::of(&nowhere), None, "t", &log()).await.err().unwrap();
        assert!(e.contains("port=1"), "{}", e);
    }

    /// [zip] an archive that is not there fails the run before anything connects
    #[tokio::test]
    async fn a_missing_archive_fails_the_run_first() {
        let c = ConfigMap::of(&[("source_zip", "/nonexistent/scripts.zip"), ("db_host", "127.0.0.1"), ("db_port", "1")]);
        let e = migrate(c, false, &log()).await.unwrap_err();
        assert!(e.starts_with("SGE-E602 "), "{}", e);
    }
}
