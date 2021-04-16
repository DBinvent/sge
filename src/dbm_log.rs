use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::process::Command;
use std::result::Result;

use crypto_hash::Algorithm;
use postgres::{Client, Error, Transaction};
use serde::Serialize;
use slog::Logger;

use crate::dbm_dbc::InfoSchemaType;
use crate::dbm_yml::{ConfigMap, FilesMap};
use crate::BASELINE;
use std::fs::OpenOptions;
use std::time::Instant;

/// loaded from DB or stored to DB, see [STATEMENT_LOG_TBL] and [STATEMENT_LOG_TABLE_YML] for table defenition
#[derive(Debug, Clone)]
pub(crate) struct StatementLog {
    /// [transient] log: schema.table
    pub table: String,
    /// [transient] will be used to calculate took sec
    pub started: Instant,
    /// [transient] to ignode on save
    pub dry_run: bool,
    pub id: u64,
    /// filename
    pub source: String,
    /// target is a database name, or empty string if the log schema on same database
    /// or environment(dev|prod)+host(node)+database for orchestration configuration
    pub target: String,
    pub source_type: char,
    pub version: String,
    pub checksum: String,
    /// set of schemas i.e. module (microservice)
    pub catalog: String,
    pub script_tag: String,
    // pub created_on & executed_on: Timestamp<?>,
    pub executed_by: String,
    pub ok: bool,
    pub msg: String,
    pub took_sec: i64,
}

/// information schema types: schema, table, column
pub(crate) type LogSchemaType = HashMap<String, (bool, String)>;

impl StatementLog {
    /// select table for update -- row level lock
    #[inline]
    pub(crate) fn load_all(t: &mut Transaction, cfg: &ConfigMap) -> Result<LogSchemaType, String> {
        let db = cfg.target();
        let log_st = format!("{}.{}", cfg.schema(), cfg.history_table_name());
        let dry_run = cfg.get_bool("dry_run", true);
        let lock = format!(
            "select version from {} where target = '{}' and source_type='S' \
            and source = $1 {}",
            log_st,
            BASELINE,
            if dry_run { "" } else { "for update" }
        );
        let mut res = HashMap::new();
        match t.query(lock.as_str(), &[&db]) {
            Ok(_) => {
                let sql = format!(
                    "select source_type||version, ok, checksum from {} where target = $1 and ok",
                    log_st
                );
                match t.query(sql.as_str(), &[&db]) {
                    Ok(v) => {
                        for r in v {
                            let key: String = r.get(0);
                            let val: bool = r.get(1);
                            let checksum: String = r.get(2);
                            res.insert(key, (val, checksum));
                        }
                        Ok(res)
                    }
                    Err(e) => Err(format!("trying select: [{}] {}", sql, e)),
                }
            }
            Err(e) => {
                if dry_run {
                    Ok(res)
                } else {
                    Err(format!("trying select [{}] to lock: {}", lock, e))
                }
            }
        }
    }
    /// create a default [StatementLog] pointed to schema-table (transient field)
    pub(crate) fn new(cfg: &ConfigMap) -> Self {
        StatementLog {
            table: cfg.history_table_name(),
            started: Instant::now(),
            dry_run: cfg.get_bool("dry_run", false),
            id: 0,
            source: "baseline".to_string(),
            target: cfg.target(),
            source_type: ' ',
            version: "0".to_string(),
            checksum: "".into(),
            catalog: cfg.get("db_catalog", ""),
            script_tag: cfg.get("script_tag", ""),
            executed_by: cfg.get("executed_by", ""),
            ok: false,
            msg: "".to_string(),
            took_sec: 0,
        }
    }

    /// create an instance [StatementLog]
    pub(crate) fn instance(&self, code: char, version: &String, file: &String) -> Self {
        let mut insta = self.clone();
        insta.source_type = code;
        insta.version = version.clone();
        insta.source = file.clone();
        insta.checksum = if file.as_str() == BASELINE {
            "".into()
        } else {
            match fs::File::open(file) {
                Ok(mut file) => hash_it(&mut file),
                Err(e) => e.to_string(),
            }
        };
        insta.started = Instant::now();
        insta
    }

    /// deploy after file if set
    #[inline]
    pub(crate) fn after(
        &self,
        key: &str,
        yschemas: &FilesMap,
        logs: &LogSchemaType,
        pgsrv: &str,
        cfg: &ConfigMap,
        t: &mut Transaction,
        log: &Logger,
    ) -> Result<(), String> {
        if let Some((va, afile)) = yschemas.after.get_key_value(&cfg.get(key, "")) {
            self.instance('A', va, afile)
                .deploy(&logs, pgsrv, cfg, t, log)
        } else {
            Ok(())
        }
    }

    /// deploy a file by psql call and store results
    #[inline]
    pub(crate) fn deploy(
        &self,
        logs: &LogSchemaType,
        pgsrv: &str,
        cfg: &ConfigMap,
        t: &mut Transaction,
        log: &Logger,
    ) -> Result<(), String> {
        let key = format!("{}{}", self.source_type, self.version);
        if let Some((ok, checks)) = logs.get(&key) {
            if *ok {
                if self.checksum == *checks {
                    return Ok(());
                } else if self.source_type == 'V' {
                    if cfg.get_bool("version_checksum", false) {
                        // checksum different
                        return Err(format!(
                            "Version [{}] script file checksum deployment mismatch",
                            self.version
                        ));
                    } else {
                        return Ok(());
                    }
                }
            }
        };
        if self.dry_run {
            self.dry_write(cfg, log)
        } else {
            let mut cmd = Command::new("psql");
            if cfg.is_pgsrv() {
                cmd.env("PGSERVICEFILE", pgsrv);
            }
            if let Some(port) = cfg.get_opt("db_port") {
                cmd.arg(format!("-p{}", port));
            }
            match cmd
                .arg(format!("--dbname={}", cfg.get("db_name", "")))
                .arg("-X")
                .arg("-1")
                .arg("-f")
                .arg(&self.source)
                .output()
            {
                Ok(output) => {
                    let msg = format!(
                        "{} {}",
                        String::from_utf8_lossy(output.stderr.as_ref()),
                        String::from_utf8_lossy(output.stdout.as_ref())
                    );
                    if output.status.success() {
                        info!(log, "{}: OK\n{}", &self.source, msg);
                        self.persist(None, t)
                    } else {
                        error!(log, "{} : {}", &self.source, msg);
                        self.persist(Some(msg), t)
                    }
                }
                Err(e) => {
                    error!(log, "{} {}", &self.source, e);
                    self.persist(Some(e.to_string()), t)
                }
            }
        }
    }

    #[inline]
    pub(crate) fn persist(&self, err: Option<String>, t: &mut Transaction) -> Result<(), String> {
        if self.dry_run {
            return match err {
                None => Ok(()),
                Some(e) => Err(e), // forward error
            };
        }
        let took = self.started.elapsed().as_secs();
        let took_sec = if took > i64::MAX as u64 {
            i64::MAX
        } else {
            took as i64
        };
        let (ok, msg) = match &err {
            None => (true, "".into()),
            Some(e) => (false, e.clone()),
        };
        match t.query(
            format!(
                "select id from {} where target = $1 and source_type = $2 and version = $3",
                self.table
            )
            .as_str(),
            &[&self.target, &self.source_type.to_string(), &self.version],
        ) {
            Ok(rows) => {
                match if rows.len() > 0 {
                    let id: i32 = rows[0].get(0);
                    t.execute(format!("update {} set ok = $1, msg = $2, took_sec = $3, executed_by = $4,\
                        checksum = $5, script_tag = $6, catalog = $7, executed_on=current_timestamp where id = $8",
                        self.table).as_str(), &[
                            &ok, &msg, &took_sec, &self.executed_by, &self.checksum, &self.script_tag,& self.catalog,
                            &id])
                } else {
                    t.execute(format!("insert into {} (target, source_type, source, version, ok, msg, took_sec, executed_by, \
                        checksum, script_tag, catalog, executed_on, created_on) values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11, current_timestamp, current_timestamp)",
                        self.table).as_str(), &[&self.target, &self.source_type.to_string(), &self.source.to_string(), &self.version,
                            &ok, &msg, &took_sec, &self.executed_by, &self.checksum, &self.script_tag, &self.catalog,
                        ])
                } {
                    Ok(_) => {
                        match err {
                            None => Ok(()),
                            Some(e) => Err(e), // forward error
                        }
                    }
                    Err(e) => self.to_err(err, e),
                }
            }
            Err(e) => self.to_err(err, e),
        }
    }

    #[inline]
    fn to_err(&self, err: Option<String>, e: Error) -> Result<(), String> {
        match err {
            None => Err(e.to_string()),
            Some(er) => Err(format!("{} {}", er, e)),
        }
    }

    #[inline]
    fn dry_write(&self, cfg: &ConfigMap, log: &Logger) -> Result<(), String> {
        StatementLog::dry_store(
            cfg.get_opt("dry_run_to"),
            format!("\\i {} \n", self.source),
            log,
        )
    }

    #[inline]
    pub(crate) fn dry_store(file: Option<String>, sql: String, log: &Logger) -> Result<(), String> {
        match &file {
            None => {
                info!(log, "\n{}", sql);
                Ok(())
            }
            Some(f) => {
                match OpenOptions::new()
                    .write(true)
                    .append(true)
                    .create(true)
                    .open(f)
                {
                    Ok(mut file) => match file.write(sql.as_bytes()) {
                        Ok(i) => {
                            let _ = file.sync_all();
                            debug!(log, "generated: {} bytes to {}", i, f);
                            Ok(())
                        }
                        Err(e) => Err(format!("dump sql to [{}]: {}", f, e)),
                    },
                    Err(e) => Err(format!("dump sql to [{}]: {}", f, e)),
                }
            }
        }
    }
}

const BUFFER_SIZE: usize = 1024;

/// Compute digest value for given `Reader` and print it
/// On any error simply return without doing anything
fn hash_it<R: Read>(reader: &mut R) -> String {
    let mut hasher = crypto_hash::Hasher::new(Algorithm::MD5);
    let mut buffer = [0u8; BUFFER_SIZE];
    loop {
        let n = match reader.read(&mut buffer) {
            Ok(n) => n,
            Err(_) => return "".into(),
        };
        let _ = hasher.write(&buffer[..n]);
        if n == 0 || n < BUFFER_SIZE {
            break;
        }
    }
    base64::encode(hasher.finish())
}

/// load impl
#[inline]
pub(crate) fn load_impl(
    log_client: &mut Client,
    cfg: &ConfigMap,
    _log: &Logger,
) -> Result<String, String> {
    let dbc = crate::dbm_dbc::load_info_schema(log_client, cfg)?;
    let mut t = log_client.transaction().unwrap(); // begin
    let yschemas = crate::dbm_yml::load_all(cfg)?;
    let logs = StatementLog::load_all(&mut t, cfg)?;
    let _ = t.rollback();
    let res = LoadResult {
        db_info_schema: dbc,
        scripts: yschemas,
        history: logs,
    };
    match serde_json::to_string(&res) {
        Ok(data) => Ok(data),
        Err(e) => Err(format!("{}", e)),
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LoadResult {
    pub db_info_schema: InfoSchemaType,
    pub scripts: FilesMap,
    pub history: LogSchemaType,
}
