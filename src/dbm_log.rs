use std::{fmt, fs};
use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::fmt::Display;
use std::fs::File;
use std::io::{Read, Write};
use std::process::{Command, Output};
use std::result::Result;

use base64::DecodeError;
use crypto_hash::Algorithm;
use lazy_static::lazy_static;
use postgres::{Client, Error, Row, Transaction};
use postgres::types::Timestamp;
use serde::{Deserialize, Serialize};
use slog::Logger;

use crate::BASELINE;
use crate::dbm_yml::{ConfigDefMap, ConfigMap, YTable, FilesMap};
use std::time::Instant;
use crate::dbm_dbc::IS_TYPE;

/// loaded from DB or stored to DB, see [STATEMENT_LOG_TBL] and [STATEMENT_LOG_TABLE_YML] for table defenition
#[derive(Debug, Clone)]
pub(crate) struct StatementLog {
    /// [transient] log: schema.table
    pub table: String,
    /// [transient] will eb used to calculate took sec
    pub started: Instant,
    pub id: u64, 		//\": (ct:\"serial\", dfn: \"PK\", pk: Some(true), dsr: \"PK\"),
    /// filename
    pub source: String, //\": (ct:\"varchar(250)\", dfn: \"\", dsr: \"\"),
    /// target is a database name, or empty string if the log schema on same database
    /// or environment(dev|prod)+host(node)+database for orchestration configuration
    pub target: String, //\": (ct:\"varchar(150)\", dfn: \"\", dsr: \"\"),
    pub source_type: char, //\": (ct:\"char\", dfn: \"\", dsr: \"S: Schema, V: Version, R: Re-runnable\"),
    pub version: String, //\": (ct:\"varchar(50)\", dfn: \"\", dsr: \"\"),
    // pub created: Timestamp<u64>, //\": (ct:\"timestamp\", dfn: \"default current_timestamp()\", dsr: \"\"),
    pub checksum: String, //\": (ct:\"bigint\", dfn: \"\", dsr: \"\"),
    /// set of schemas i.e. module (microservice)
    pub catalog: String, //\": (ct:\"timestamp\", dfn: \"\", dsr: \"\"),
    pub script_tag: String, //\": (ct:\"varchar(150)\", dfn: \"\", dsr: \"\"),
    // pub executed_on: Timestamp<u64>, //\": (ct:\"timestamp\", dfn: \"\", dsr: \"\"),
    pub executed_by: String, //\": (ct:\"varchar(50)\", dfn: \"\", dsr: \"\"),
    pub ok: bool, 		// dfn: \"not null default false\", dsr: \"Successful execution of the script\"),
    pub msg: String, 	//\": (ct:\"varchar(250)\", dfn: \"\", dsr: \"\"),
    pub took_sec: i64, //\": (ct:\"bigint\", dfn: \"\", dsr: \"\"),
}

/// information schema types: schema, table, column
pub(crate) type LS_TYPE = HashMap<String, (bool, String)>;

impl StatementLog {
    /// select table for update -- row level lock
    #[inline]
    pub(crate) fn load_all(t: &mut Transaction, log_st: &String, cfg: &ConfigMap) -> Result<LS_TYPE, String> {
        let db = cfg.target();
        let lock = format!("select version from {} where target = '{}' and source_type='S' and source = '{}' for update", log_st, db, BASELINE);
        match t.query(lock.as_str(), &[]) {
            Ok(r) => {
                let mut res = HashMap::new();
                let sql = format!("select source_type||version, ok, checksum from {} where target = '{}' where ok", log_st, db);
                match t.query(sql.as_str(), &[]) {
                    Ok(v) => {
                        for r in v {
                            let key: String = r.get(0);
                            let val: bool = r.get(1);
                            let checksum: String = r.get(2);
                            res.insert(key, (val, checksum));
                        }
                        Ok(res)
                    }
                    Err(e) => {
                        Err(format!("trying select: {}", e))
                    }
                }
            }
            Err(e) => {
                Err(format!("trying select to lock: {}", e))
            }
        }
    }
    /// create a default [StatementLog] pointed to schema-table (transient field)
    pub(crate) fn new(log_st: &String, cfg: &ConfigMap) -> Self {
        StatementLog {
            table: log_st.clone(),
            started: Instant::now(),
            id: 0,
            source: "baseline".to_string(),
            target: cfg.target(),
            source_type: ' ',
            version: "0".to_string(),
            checksum: "".into(),
            catalog: cfg.get("db_catalog", "") ,
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
        insta.checksum = match fs::File::open(file) {
            Ok(mut file) => hash_it(&mut file),
            Err(e) => e.to_string()
        };
        insta.started = Instant::now();
        insta
    }

    /// deploy a file by psql call and store results
    #[inline]
    pub(crate) fn deploy(&self, logs: &LS_TYPE, pgsrv: &str, cfg: &ConfigMap, t: &mut Transaction, log: &Logger, ) -> Result<(), String> {
        let key = format!("{}{}", self.source_type, self.version);
        if let Some((ok, checks)) = logs.get(&key) {
            if *ok {
                if self.checksum == *checks {
                    return Ok(());
                } else if self.source_type == 'V' {
                    if cfg.get_bool("version_checksum", false) {
                        // checksum different
                        return Err(format!("Version [{}] script file checksum deployment mismatch", self.version));
                    } else {
                        return Ok(());
                    }
                }
            }
        };
        let mut cmd = Command::new("psql");
        if let Ok(pwd) = cfg.get_opt("db_password") {
            cmd.env("PGSERVICEFILE", pgsrv);
        }
        if let Ok(port) = cfg.get_opt("db_port") {
            cmd.arg(format!("-p{}", port));
        }
        match cmd.arg(format!("--dbname={}", cfg.get("db_name", "")))
            .arg("-X").arg("-1").arg("-f").arg(&self.source).output() {
            Ok(output) => {
                let msg = format!("{} {}", String::from_utf8_lossy(output.stderr.as_ref()),
                                  String::from_utf8_lossy(output.stdout.as_ref()));
                if output.status.success() {
                    info!(log, "OK\n{}", msg);
                    self.persist( None, t)
                } else {
                    error!(log, "{}", msg);
                    self.persist(Some(msg), t)
                }
            }
            Err(e) => {
                error!(log, "{}", e);
                self.persist(Some(e.to_string()), t)
            }
        }
    }

    #[inline]
    pub(crate) fn persist(&self, err: Option<String>, t: &mut Transaction)  -> Result<(), String> {
        let took = self.started.elapsed().as_secs();
        let took_sec = if took > i64::MAX as u64 { i64::MAX } else { took as i64};
        let (ok, msg) = match &err {
            None => (true, "".into()),
            Some(e) => (false, e.clone())
        };
        match t.query(format!("select id from {} where target = $1 and source_type = $2 and version = $3",
            self.table).as_str(), &[&self.target, &self.source_type.to_string(), &self.version]) {
            Ok(rows) => {
                match if rows.len()>0{
                    let id:i64 = rows[0].get(0);
                    t.execute(format!("update {} set ok = $1, msg = $2, took_sec = $3, executed_by = $4,\
                        checksum = $5, script_tag = $6, catalog = $7, executed_on=current_timestamp where id = $8",
                        self.table).as_str(), &[
                            &ok, &msg, &took_sec, &self.executed_by, &self.checksum, &self.script_tag,& self.catalog,
                            &id])
                } else {
                    t.execute(format!("insert into {} (target, source_type, version, ok, msg, took_sec, executed_by, \
                        checksum, script_tag, catalog, executed_on) values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10, current_timestamp)",
                        self.table).as_str(), &[&self.target, &self.source_type.to_string(), &self.version,
                            &ok, &msg, &took_sec, &self.executed_by, &self.checksum, &self.script_tag,& self.catalog,
                        ])
                } {
                    Ok(_) => {
                        match err {
                            None => Ok(()),
                            Some(e) => Err(e) // forward error
                        }
                    }
                    Err(e) =>  self.to_err(err, e)
                }
            }
            Err(e) => self.to_err(err, e)
        }
    }

    #[inline]
    fn to_err(&self, err: Option<String>, e: Error) -> Result<(), String>  {
        match err {
            None => Err(e.to_string()),
            Some(er) => Err(format!("{} {}", er, e))
        }
    }

}

#[inline]
fn arg(cmd: &mut Command, prefix: &str, key: &str, cfg: &ConfigMap) {
    if let Ok(v) = cfg.get_po(prefix, key) {
        cmd.arg(format!("{}={}", key, v));
    }
}

#[inline]
fn parg(prefix: &str, key: &str, cfg: &ConfigMap) -> String {
    match cfg.get_po(prefix, key) {
        Ok(v) => {
            format!("{}={}", key, v)
        }
        Err(_) => "".into()
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
        hasher.write(&buffer[..n]);
        if n == 0 || n < BUFFER_SIZE {
            break;
        }
    }
    base64::encode(hasher.finish())
}


/// load impl
#[inline]
pub(crate) fn load_impl(log_client: &mut Client, cfg: &ConfigMap, log: &Logger) -> Result<String, String> {
    let schema = cfg.get("schema", "public");
        let dbc = crate::dbm_dbc::load_info_schema(log_client, cfg)?;

    let mut t = log_client.transaction().unwrap(); // begin
    let log_st = format!("{}.{}", &schema, &crate::dbm_yml::STATEMENT_LOG_TBL.table_name);
    let yschemas = crate::dbm_yml::load_all(cfg)?;
    let logs = StatementLog::load_all(&mut t, &log_st, cfg)?;
    t.rollback();
    let res = LoadResult{
        db_info_schema: dbc,
        scripts: yschemas,
        history: logs
    };
    match serde_json::to_string(&res) {
        Ok(data) => Ok(data),
        Err(e) => Err(format!("{}", e))
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LoadResult {
    pub db_info_schema: IS_TYPE,
    pub scripts: FilesMap,
    pub history: LS_TYPE,
}
