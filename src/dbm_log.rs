use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::Command;
use std::result::Result;

use postgres::{Client, Transaction};
use serde::Serialize;
use slog::Logger;

use crate::dbm_cfg::ConfigMap;
use crate::dbm_dbc::{STable, SColumnDfn};
use crate::dbm_yml::{semver_parse, FilesMap, OrderedHashMap, YColumn, YSchema, YTable};
use crate::*;
use chrono::{DateTime, Utc};
use semver::Version;
use std::fs::{File, OpenOptions};
use std::time::Instant;
use crypto::digest::Digest;
use crate::dbm_etl::Etl;

/// loaded from DB or stored to DB, see [STATEMENT_LOG_TBL] and [STATEMENT_LOG_TABLE_YML] for table defenition
#[derive(Debug, Clone, Serialize)]
pub(crate) struct StatementLog {
    /// [transient] log: schema.table
    #[serde(skip_serializing)]
    pub table: String,
    /// [transient] will be used to calculate took sec
    #[serde(skip_serializing)]
    pub started: Instant,
    /// [transient] to ignode on save
    #[serde(skip_serializing)]
    pub dry_run: bool,
    pub id: i32,
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
    pub executed_by: String,
    pub ok: bool,
    pub msg: String,
    pub took_sec: i64,
    pub size: i64,
}

/// source_type||version: file, checksum
pub(crate) type LogSchemaType = HashMap<String, (String, String)>;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct LogVer {
    #[serde(skip)]
    pub ver: Version,
    pub version: String,
    pub successful: bool,
    pub script_tag: String,
    pub executed_by: String,
    #[serde(with = "my_date_format")]
    pub executed_on: DateTime<Utc>,
}

mod my_date_format {
    use chrono::{DateTime, TimeZone, Utc};
    use serde::{self, Deserialize, Deserializer, Serializer};

    const FORMAT: &'static str = "%Y-%m-%d %H:%M:%S";

    pub fn serialize<S>(date: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let s = format!("{}", date.format(FORMAT));
        serializer.serialize_str(&s)
    }

    #[allow(dead_code)]
    pub fn deserialize<'de, D>(deserializer: D) -> Result<DateTime<Utc>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Utc.datetime_from_str(&s, FORMAT)
            .map_err(serde::de::Error::custom)
    }
}

type SortedStatementLog = HashMap<String, HashMap<String, LogVer>>;

impl StatementLog {

    /// load impl
    #[inline]
    pub(crate) fn load_formatted(cfg: &ConfigMap, log_client: &mut Client) -> Result<String, String> {
        let logs = StatementLog::load_ok(log_client, cfg)?;
        if cfg.get("out_format").as_str() == "json" {
            serde_json::to_string(&logs)
                .map_err(|e| format!("converting result to JSON format with error: {}", e))
        } else {
            serde_yaml::to_string(&logs)
                .map_err(|e| format!("converting result to YAML format with error: {}", e))
        }
    }

    /// select table for update -- row level lock
    #[inline]
    pub(crate) fn load_all(
        t: &mut Transaction,
        dry_run: bool,
        cfg: &ConfigMap,
    ) -> Result<LogSchemaType, String> {
        let log_st = format!("{}.{}", cfg.schema(), cfg.history_table_name());
        let lock = format!(
            "select version from {} where target = '{}' and source_type='S' and source = $1 {}",
            log_st,
            BASELINE,
            if dry_run { "" } else { "for update" }
        );
        let target = cfg.target();
        let mut res = HashMap::new();
        match t.query(lock.as_str(), &[&target]) {
            Ok(_) => {
                let sql = format!(
                    "select source_type||version, source, checksum, source_type from {} where target = $1 and ok",
                    log_st
                );
                match t.query(sql.as_str(), &[&target]) {
                    Ok(v) => {
                        for r in v {
                            let key: String = r.get(0);
                            let val: String = r.get(1);
                            let checksum: String = r.get(2);
                            let source_type: String = r.get(3);
                            if source_type.as_str() == "D" {
                                res.insert(val.clone(), (val, checksum));
                            } else {
                                res.insert(key, (val, checksum));
                            }
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

    /// select table for update -- row level lock
    // map: db_name(target) -> catalog (schemas) -> max(version)/
    #[inline]
    #[allow(unused_mut)]
    fn load_ok(t: &mut Client, cfg: &ConfigMap) -> Result<SortedStatementLog, String> {
        let mut sql = format!(
            "select target, catalog, version, script_tag, executed_by, executed_on, ok from {}.{} \
             where version <> 'baseline' ",
            cfg.schema(),
            cfg.history_table_name()
        );
        cfg.write(&mut sql, "_db", "target", "and");
        cfg.write(&mut sql, "_db", "catalog", "and");
        sql.insert_str(sql.len() - 1, " order by executed_on, id");
        match t.query(sql.as_str(), &[]) {
            Ok(v) => {
                let mut targets = HashMap::new();
                for r in v {
                    let target: String = r.get(0);
                    let catalog: String = r.get(1);
                    let version: String = r.get(2);
                    let script_tag: String = r.get(3);
                    let executed_by: String = r.get(4);
                    let executed_on: DateTime<Utc> = r.get(5);
                    let successful: bool = r.get(6);

                    let sl = LogVer {
                        ver: match semver_parse(version.as_str()) {
                            Ok(v) => v,
                            Err(_) => Version::new(0, 0, 0),
                        },
                        version,
                        successful,
                        script_tag,
                        executed_by,
                        executed_on,
                    };

                    match targets.get_mut(&target) {
                        None => {
                            let mut catalogs = HashMap::new();
                            let t = target.clone();
                            catalogs.insert(catalog.clone(), sl);
                            targets.insert(t, catalogs);
                        }
                        Some(mut ts) => match ts.get(catalog.as_str()) {
                            None => {
                                ts.insert(catalog.clone(), sl);
                            }
                            Some(s) => {
                                if sl.ver > s.ver {
                                    ts.insert(catalog.clone(), sl);
                                }
                            }
                        },
                    }
                }

                Ok(targets)
            }
            Err(e) => Err(format!("trying select: [{}] {}", sql, e)),
        }
    }

    /// create a default [StatementLog] pointed to schema-table (transient field)
    pub(crate) fn new(cfg: &ConfigMap) -> Self {
        StatementLog {
            table: format!("{}.{}", cfg.schema(), cfg.history_table_name()),
            started: Instant::now(),
            dry_run: cfg.get_bool("dry_run", false),
            id: 0,
            source: "baseline".to_string(),
            target: tlim(&cfg.target(), SRC_T_M_LEN),
            source_type: ' ',
            version: "0".to_string(),
            checksum: "".into(),
            catalog: tlim(&cfg.get("db_catalog"), SLT_C_LEN),
            script_tag: tlim(&cfg.get("script_tag"), SLT_C_LEN),
            executed_by: tlim(&cfg.get("executed_by"), SLT_C_LEN),
            ok: false,
            msg: "".to_string(),
            took_sec: 0,
            size: 0
        }
    }

    /// create an instance [StatementLog]
    pub(crate) fn instance(&self, code: char, version: &String, dir: &String, file: &String) -> Self {
        let mut insta = self.clone();
        insta.source_type = code;
        insta.version = tlim(version, SLT_C_LEN);
        insta.source = tlim(file, SRC_T_M_LEN);
        insta.checksum = if file.as_str() == BASELINE || code == 'a' || code == 'A' {
            "".into()
        } else {
            let file = if dir.len() > 0 {
                format!("{}{}{}", dir, if dir.as_bytes()[dir.len()-1] == b'/' {""} else {"/"}, file)
            } else { file.into() };
            match fs::File::open(file) {
                Ok(mut file) => {
                    if let Ok(meta) = file.metadata() {
                        insta.size = meta.len() as i64;
                    }
                    hash_md5(&mut file)
                },
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
        files: &FilesMap,
        logs: &LogSchemaType,
        pgsrv: &str,
        cfg: &ConfigMap,
        t: Option<&mut Transaction>, // ids: &Vec<i32>,
        log: &Logger,
    ) -> Result<(), String> {
        if let Some((va, afile)) = files.after.get_key_value(key) {
            if afile.ends_with(cfg.get(key).as_str()) {
                let _ = self.instance(' ', va, &"".into(), afile)
                    .deploy(&logs, pgsrv, cfg, t, &mut None,  false, log)?;
            }
        }
        Ok(())
    }

    /// deploy a file by psql call and store results
    #[inline]
    pub(crate) fn deploy(
        &self,
        logs: &LogSchemaType,
        pgsrv: &str,
        cfg: &ConfigMap,
        t: Option<&mut Transaction>,
        clog_t: &mut Option<Transaction>, // copy_log_client
        // _id: &Vec<i32>,
        persist: bool,
        log: &Logger,
    ) -> Result<i32, String> {
        let key = format!("{}{}", self.source_type, self.version);
        if let Some((_sl_file, checks)) = logs.get(&key) {
            if self.checksum == *checks {
                return Ok(0);
            } else if self.source_type == 'V' || self.source_type == 'P' {
                return if cfg.get_bool("checksum", false) {
                    // checksum different
                    Err(format!(
                        "Version [{}] script file [{}] checksum deployment mismatch",
                        self.version, self.source
                    ))
                } else {
                    Ok(0)
                };
            }
        };
        if self.dry_run {
            self.dry_write(cfg, log)
        } else {
            let mut cmd = Command::new("psql");
            let dir = cfg.get("source_dir");
            if dir.len() > 0 {
                cmd.current_dir(dir.as_str());
            }
            if cfg.is_pgsrv() {
                cmd.env("PGSERVICEFILE", pgsrv);
            }
            if let Some(port) = cfg.get_opt("db_port") {
                cmd.arg(format!("-p{}", port));
            }
            if let Some(user) = cfg.get_opt("db_user") {
                cmd.env("PGUSER", user);
            }
            if let Some(pgpass) = cfg.get_opt("db_password") {
                cmd.env("PGPASSWORD", pgpass);
            }
            let single_transaction = if self.source_type == 'V' {
                match cfg.get_opt("version_transaction") {
                    Some(t) => t.as_str() == "single",
                    None => find_single_transaction(self.source.as_str()),
                }
            } else {
                true
            };
            if single_transaction {
                cmd.arg("-1"); // --single-transaction
            }
            debug!(log, "executing {} {}...", self.source, match &t { None => "(no history log)", _ => ""} );
            match cmd
                .arg(format!("--dbname={}", cfg.get("db_name")))
                .arg(format!("--host={}", cfg.get("db_host")))
                .arg("-X") //  do not read startup file (~/.psqlrc)
                .arg("-v")
                .arg("ON_ERROR_STOP=1")
                .arg("-f")
                .arg(&self.source)
                .output()
            {
                Ok(output) => {
                    if output.status.success() {
                        info!(log, "{}: OK", &self.source);
                        let msg = trim_output(&output.stdout);
                        if msg.len() > 0 {
                            info!(log, "[{}]", msg);
                        }
                        let emsg = trim_output(&output.stderr);
                        if emsg.len() > 0 {
                            if persist {
                                error!(log, "psql({})> {}", self.source, emsg);
                            } else {
                                info!(log, "psql({})> {}", self.source, emsg);
                            }
                        }
                        if persist {
                            self.persist(None, t, clog_t)
                        } else { Ok(0)}
                    } else {
                        let msg = format!(
                            "{} : {} {}",
                            &self.source,
                            trim_output(&output.stdout),
                            trim_output(&output.stderr)
                        );
                        error!(log, "{}", msg);
                        if persist {
                            self.persist(Some(msg), t, clog_t)
                        } else { Ok(0)}
                    }
                }
                Err(e) => {
                    error!(log, "psql({}) {}", self.source, e);
                    if persist {
                        self.persist(Some(e.to_string()), t, clog_t)
                    } else { Ok(0)}
                }
            }
        }
    }

    #[inline]
    pub(crate) fn persist(&self, err: Option<String>,
        t: Option<&mut Transaction>,
        c: &mut Option<Transaction>, // copy
    ) -> Result<i32, String> {
        if let Some(x) = c {
            let _ = self.persist_into(err.clone(), Some(x));
        }
        self.persist_into(err, t)
    }

    #[inline]
    pub(crate) fn persist_into(&self, err: Option<String>,
        t: Option<&mut Transaction>,
    ) -> Result<i32, String> {
        let t = match t {
            None => {return Ok(0);}
            Some(t) => t,
        };
        if self.dry_run {
            return match err {
                None => Ok(0),
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
            Some(e) => (false, tlim(e, SRC_T_M_LEN)),
        };
        let sql = format!( "select id from {} where \
            (source_type <> 'D' and target = $1 and source_type = $2 and version = $3) OR \
            (source_type = 'D' and source = $4)", self.table);
        let rows = t.query(sql.as_str(),
            &[&self.target, &self.source_type.to_string(), &self.version, &self.source],
        ).map_err(|e| format!("{}: {}", sql, e))?;
        let id = if rows.len() > 0 {
            let id: i32 = rows[0].get(0);
            let sql = format!(
"update {} set ok = $1, msg = $2, took_sec = $3, executed_by = $4,\
 checksum = $5, script_tag = $6, catalog = $7, source = $8, size = $9, \
 executed_on=current_timestamp, times = times+1 where id = $10",
                self.table);
            let _ = t.execute(sql.as_str(), &[
                    &ok, &msg, &took_sec, &self.executed_by, &self.checksum, &self.script_tag, &self.catalog, &self.source, &self.size,
                    &id]).map_err(|e| format!("{}: {}", sql, e))?;
            id
        } else {
            let sql = format!(
"insert into {} (target, source_type, source, version, ok, msg, took_sec, executed_by, \
 checksum, script_tag, catalog, size, executed_on, created_on) values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12, \
 current_timestamp, current_timestamp) returning id",
                self.table);
            let r = t.query_one(sql.as_str(), &[&self.target, &self.source_type.to_string(), &self.source, &self.version,
                    &ok, &msg, &took_sec, &self.executed_by, &self.checksum, &self.script_tag, &self.catalog, &self.size,
                ]).map_err(|e| format!("{}: {}", sql, e))?;
            let id: i32 = r.get(0);
            id
        };
        match err {
            None => Ok(id),
            Some(e) => Err(e)
        }
    }

    #[inline]
    fn dry_write(&self, cfg: &ConfigMap, log: &Logger) -> Result<i32, String> {
        StatementLog::dry_store(
            cfg.get_opt("dry_run_to"),
            format!("\\i [{}]{} \n", cfg.get("source_dir"), self.source),
            log,
        )
    }

    #[inline]
    pub(crate) fn dry_store(file: Option<String>, sql: String, log: &Logger) -> Result<i32, String> {
        match &file {
            None => {
                if sql.len() > 0 {
                    info!(log, "\n{}", sql);
                }
                Ok(0)
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
                            Ok(0)
                        }
                        Err(e) => Err(format!("dump sql to [{}]: {}", f, e)),
                    },
                    Err(e) => Err(format!("dump sql to [{}]: {}", f, e)),
                }
            }
        }
    }
}

#[inline]
fn trim_output(input: &Vec<u8>) -> String {
    String::from_utf8_lossy(input)
        .trim_matches(|c| c == ' ' || c == '\n' || c == '^')
        .into()
}

const SINGLE_TRANSACTION: &str = "--single-transaction=";

fn find_single_transaction(file: &str) -> bool {
    let mut single_transaction: Option<bool> = None;
    if let Ok(file) = File::open(file) {
        let reader = BufReader::new(file);
        for line in reader.lines() {
            match line {
                Ok(l) => {
                    let mut prev = ' ';
                    let mut i = 0usize;
                    for c in l.chars() {
                        if c != ' ' {
                            if i + SINGLE_TRANSACTION.len() + 4 > l.len() {
                                break;
                            }
                            if c == '-' && prev == '-' {
                                // found comment line - search for comment
                                if let Some(x) = l.find(SINGLE_TRANSACTION) {
                                    let x = x + SINGLE_TRANSACTION.len() + 5;
                                    // set only if found, otherwise search until sql code starts
                                    if x <= l.len() {
                                        let cm = if x == l.len() {
                                            &l[x - 5..]
                                        } else {
                                            &l[x - 5..x]
                                        };
                                        single_transaction = Some(cm != "false");
                                    } else {
                                        single_transaction = Some(true);
                                    }
                                }
                                break;
                            }
                            if prev == ' ' && c.is_alphabetic() {
                                // found non comment line - breaking search
                                single_transaction = None;
                                break;
                            }
                        }
                        prev = c;
                        i += 1;
                    }
                    if let Some(_) = single_transaction {
                        break;
                    }
                }
                Err(_) => {
                    break;
                }
            }
        }
    }
    match single_transaction {
        None => true,
        Some(s) => s,
    }
}

/// Trim to limit
#[inline]
fn tlim(value: &String, max: usize) -> String {
    if value.len() >= max {
        value[0..max].into()
    } else {
        value.clone()
    }
}

const BUFFER_SIZE: usize = 1024;

/// Compute digest value for given `Reader` and print it
/// On any error simply return without doing anything
fn hash_md5<R: Read>(reader: &mut R) -> String {
    let mut hasher = crypto::md5::Md5::new();
    let mut buffer = [0u8; BUFFER_SIZE];
    loop {
        let n = match reader.read(&mut buffer) {
            Ok(n) => n,
            Err(_) => return "".into(),
        };
        let _ = hasher.input(&buffer[..n]);
        if n == 0 || n < BUFFER_SIZE {
            break;
        }
    }
    let mut output = [0; 16]; // An MD5 is 16 bytes
    hasher.result(&mut output);
    base64::encode(output)
}


#[derive(Debug, Clone, Serialize)]
pub(crate) struct LoadResult {
    pub database: Vec<YSchema>,
}

impl LoadResult {
    pub(crate) fn load(etl: &Etl, db_client: &mut Client, log: &Logger) -> Result<Self, String> {
        let input = crate::dbm_dbc::load_info_schema(etl.db_name.as_str(), db_client)?;
        let owners = crate::dbm_dbc::load_info_schema_owner(etl.db_name.as_str(),db_client)?;

        let mut schemas = Vec::new();
        for (schema, tables) in input {
            let mut tables = tables.values().cloned().collect::<Vec<STable>>();
            tables.sort_by(|a,b| a.sort_order.cmp(&b.sort_order));
            let mut ytables = OrderedHashMap::new();
            for  table in &tables {
                let mut yt = YTable {
                    table_name: table.table_name.clone(),
                    description: table.table_comment.clone().unwrap_or("".to_string()),
                    transaction: "".to_string(),
                    sql: "".to_string(),
                    constraint: "".to_string(),
                    columns: OrderedHashMap::new(),
                    triggers: OrderedHashMap::new(),
                    etl: None,
                    weight: 0,
                };
                let mut columns = table.columns.values().cloned().collect::<Vec<SColumnDfn>>();
                columns.sort_by(|a,b| a.sort_order.cmp(&b.sort_order));
                for c in columns {
                    let yc = YColumn {
                        name: c.column_name.into(),
                        column_type: c.column_type.clone(),
                        default_value: c.column_default.clone(),
                        constraint: if !c.nullable || c.pk || c.fk.is_some() { Some(YConstr {
                            primary_key: if c.pk { Some(true) } else { None },
                            nullable: c.nullable,
                            foreign_key: c.fk.clone()
                                .map( |f| YForeignKey{ references: f.0.clone(), sql: f.1.clone() })
                        }) }
                        else { None },
                        description: c.column_comment.clone().unwrap_or("".to_string()),
                        sql: "".to_string(),
                    };
                    let _ = yt.columns.append(yc);
                }
                let _ = ytables.append(yt);
            }
            let ys = YSchema {
                schema_name: schema.clone(),
                owner: match owners.get(&schema) {
                    None => "".to_string(),
                    Some((o, _)) => o.clone(),
                },
                tables: ytables,
                file: "".to_string(),
            };
            schemas.push(ys);
        }
        let mut lr = LoadResult { database: schemas };
        let _ = etl.extract(&mut lr, db_client, log)?;
        Ok(lr)
    }

    /// load impl
    #[inline]
    pub(crate) fn format(&self, yaml_format: bool) -> Result<String, String> {
        if yaml_format {
            serde_yaml::to_string(self)
                .map_err(|e| format!("converting result to YAML format with error: {}", e))
        } else {
            serde_json::to_string(self)
                .map_err(|e| format!("converting result to JSON format with error: {}", e))
        }
    }

}
// LoadResult

// V1_1__sample_ddl.sql

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn v_test() {
        let test_ctx_dir = "./tests/test_scen/case1";
        assert!(!find_single_transaction(format!("{}/P1_1__data.sql", test_ctx_dir).as_str()));
        assert!(!find_single_transaction(format!("{}/V1_1__sample_ddl.sql", test_ctx_dir).as_str()));
        assert!(find_single_transaction(format!("{}/R1_1__data.sql", test_ctx_dir).as_str()));
    }
}
