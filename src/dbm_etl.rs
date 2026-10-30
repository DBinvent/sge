use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;
use std::{fmt, fs};
use std::cmp::Ordering;
use std::fs::File;
use std::path::Path;
use chrono::{DateTime, Utc};
use csv::{Reader, StringRecord};
use tokio_postgres::{Row, Transaction};
use tokio_postgres::types::Type;
use schema_guard_tokio::loader::InfoSchemaType;
use schema_guard_tokio::schema::Schema;
use schema_guard_tokio::table::Table;
use schema_guard_tokio::utils::{Named, OrderedHashMap};
use semver::Version;
use slog::Logger;
use crate::{ConfigMap,  FileVersion, LoadResult, LogSchemaType, StatementLog};
use serde::Serialize;
use crate::dbm_files::FileFilter;

/// | `chrono::NaiveDateTime`         | TIMESTAMP                           |
/// | `chrono::DateTime<Utc>`         | TIMESTAMP WITH TIME ZONE            |
/// | `chrono::DateTime<Local>`       | TIMESTAMP WITH TIME ZONE            |
/// | `chrono::DateTime<FixedOffset>` | TIMESTAMP WITH TIME ZONE            |
/// | `chrono::NaiveDate`             | DATE                                |
/// | `chrono::NaiveTime`             | TIME                                |
/// | `time::PrimitiveDateTime`       | TIMESTAMP                           |
/// | `time::OffsetDateTime`          | TIMESTAMP WITH TIME ZONE            |
/// | `time::Date`                    | DATE                                |
/// | `time::Time`                    | TIME                                |
/// https://docs.rs/postgres/0.19.2/postgres/types/struct.Type.html
#[inline]
fn as_string(row: &Row, idx: usize, t: &Table) -> Result<String, String> {
    let c = &row.columns()[idx];
    let nullable = match t.columns.get(&c.name().to_string()) {
        None => {return Err(tr!("etl.column_missing", column = c.name()));}
        Some(l) => match &l.constraint {
            None => true,
            Some(s) => s.nullable
        }
    };
    let v =  if nullable {
        match c.type_() {
            &Type::INT2 => match row.get::<_, Option<i16>>(idx){
                None => "".to_string(),
                Some(x) => x.to_string()
            },
            &Type::INT4 => match row.get::<_, Option<i32>>(idx){
                None => "".to_string(),
                Some(x) => x.to_string()
            },
            &Type::INT8 => match row.get::<_, Option<i64>>(idx){
                None => "".to_string(),
                Some(x) => x.to_string()
            },
            &Type::FLOAT4 => match row.get::<_, Option<f32>>(idx){
                None => "".to_string(),
                Some(x) => x.to_string()
            },
            &Type::FLOAT8 => match row.get::<_, Option<f64>>(idx){
                None => "".to_string(),
                Some(x) => x.to_string()
            },
            &Type::BOOL => match row.get::<_, Option<bool>>(idx){
                None => "".to_string(),
                Some(x) => x.to_string()
            },
            &Type::DATE => match row.get::<_, Option<chrono::NaiveDate>>(idx){
                None => "".to_string(),
                Some(x) => x.to_string()
            },
            &Type::TIME => match row.get::<_, Option<chrono::NaiveTime>>(idx){
                None => "".to_string(),
                Some(x) => x.to_string()
            },
            &Type::TIMESTAMP => match row.get::<_, Option<chrono::NaiveDateTime>>(idx){
                None => "".to_string(),
                Some(x) => x.to_string()
            },
            &Type::TIMESTAMPTZ => match row.get::<_, Option<DateTime<Utc>>>(idx){
                None => "".to_string(),
                Some(x) => x.to_string()
            },
            t @ _ => match row.try_get::<_, Option<String>>(idx)
                .map_err(|e| tr!("etl.convert", pg_type = t.name(), err = crate::dbm_pg::pg(&e)))? {
                None => NULL.to_string(),
                // do escape
                Some(v) => match v.as_str() {
                    NULL => NULL1.to_string(),
                    NULL1 => NULL2.to_string(),
                    x @ _ => x.to_string()
                }
            }

        }
    } else {
        match c.type_() {
            &Type::INT2 => row.get::<_, i16>(idx).to_string(),
            &Type::INT4 => row.get::<_, i32>(idx).to_string(),
            &Type::INT8 => row.get::<_, i64>(idx).to_string(),
            &Type::FLOAT4 => row.get::<_, f32>(idx).to_string(),
            &Type::FLOAT8 => row.get::<_, f64>(idx).to_string(),
            &Type::BOOL => row.get::<_, bool>(idx).to_string(),
            &Type::DATE => row.get::<_, chrono::NaiveDate>(idx).to_string(),
            &Type::TIME => row.get::<_, chrono::NaiveTime>(idx).to_string(),
            &Type::TIMESTAMP => row.get::<_, chrono::NaiveDateTime>(idx).to_string(),
            &Type::TIMESTAMPTZ => row.get::<_, DateTime<Utc>>(idx).to_string(),
            t @ _ => row.try_get::<_, String>(idx)
                .map_err(|e| tr!("etl.convert", pg_type = t.name(), err = crate::dbm_pg::pg(&e)))?
        }
    };
    Ok(v)
}

const NULL:&str = "NULL";
const NULL1:&str = "\\NULL";
const NULL2:&str = "\\\\NULL";

#[inline]
fn unesc_null(input: String) -> Option<String> {
    match input.as_str() {
        NULL => None,
        NULL1 => Some(NULL.to_string()),
        NULL2 => Some(NULL1.to_string()),
        _ => Some(input)
    }
}

#[inline]
#[allow(dead_code)]
fn is_quoted(t: &Type) -> bool {
    match t {
        &Type::INT2 |
        &Type::INT4 |
        &Type::INT8 |
        &Type::FLOAT4 |
        &Type::FLOAT8 => false,
        _ => true
    }
}

#[inline]
fn is_str_escapable(t: &String) -> bool {
    let t = t.to_lowercase();
    t.contains("text") || t.contains("char")
}

#[derive(Debug, Clone)]
pub(crate) enum EtlRule {
/// - clean: Insert OR Update field if record identified by primary key is present, remove from table fields are not in file.
    Clean,
/// - force: Insert OR Update field if record identified by primary key is present.
    Force,
/// - apply: Insert OR Ignore if record identified by primary key is present.
    Apply,
/// - ignore: Do not fail, if required file is absent, otherwise use 'apply' rule.
    Ignore,
/// - skip: Do not perform data load at all.
    Skip
}

impl From<String> for EtlRule {
    fn from(input: String) -> Self {
        match input.to_lowercase().as_str() {
            "clean" => EtlRule::Clean,
            "force" => EtlRule::Force,
            "ignore" => EtlRule::Ignore,
            "skip" => EtlRule::Skip,
            _ => EtlRule::Apply,
        }
    }
}

impl Display for EtlRule {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", match &self {
            EtlRule::Clean => "clean",
            EtlRule::Force => "force",
            EtlRule::Apply => "apply",
            EtlRule::Ignore => "ignore",
            EtlRule::Skip => "skip",
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct EtlTable {
    pub schema: Option<String>,
    pub table: String,
    pub source: Option<String>,
}

impl Named for EtlTable {
    #[inline]
    fn get_name(&self) -> String {
        let mut name = String::new();
        if let Some(s) = &self.schema {
            name.push_str(s.as_str());
            name.push('.');
        }
        name.push_str(self.table.as_str());
        name
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Etl {
    pub tables: OrderedHashMap<EtlTable>,
    pub rule: EtlRule,
    pub db_name: String,
    pub dir: String,
    pub schema_dump: Option<String>,
    pub table_tr: bool,
    pub yaml_format: bool,
    /// [superset] `--snapshot_format=superset`: Apache Superset's import bundle
    pub superset: bool,
    pub ver: Option<String>,
    pub dry_run: bool,
    pub dry_run_to: Option<String>,
}

impl Etl {
    pub(crate) fn from(cfg: &ConfigMap) -> Result<Self, String> {
        let mut tables = OrderedHashMap::new();
        if let Some(data) = cfg.get_opt("snapshot_data") {
            let tbles: Vec<&str> = data.split(",").collect();
            for t in tbles {
                let t = t.to_string();
                let tv: Vec<&str> = t.split(":").collect(); // table or view as a source
                let n: Vec<&str> = tv[0].split(".").collect(); // table names
                let _ = tables.append(EtlTable {
                    schema: if n.len() > 1 { Some(n[0].to_string()) } else { None },
                    table: n[n.len() - 1].to_string(),
                    source: if tv.len() > 1 {
                        Some(tv[tv.len() - 1].to_string())
                    } else {
                        None
                    }
                });
            }
        }

        let (dir, schema_dump) = match cfg.get_opt("snapshot_to") {
            None => (cfg.get_def("source_dir", "./".to_string()), None),
            Some(f) => {
                let p = if Path::new(&f).is_dir() {
                    f.clone()
                } else {
                    match f.rfind("/") {
                        None => "./".to_string(),
                        Some(x) => f[..x].to_owned()
                    }
                };
                (p, Some(f))
            }
        };
        let dir = if dir.ends_with("/") {
            dir
        } else {
            format!("{}/", dir)
        };


        Ok(Etl {
            tables,
            rule: EtlRule::from(cfg.get("etl")),
            db_name: cfg.get("db_name"),
            dir,
            schema_dump,
            table_tr: cfg.get("schema_transaction") == "table" || cfg.is_retry(),
            yaml_format: cfg.get_def("out_format", "yaml".to_lowercase()).to_lowercase().as_str() == "yaml",
            superset: cfg.get_def("out_format", "yaml".to_string()).to_lowercase() == "superset",
            ver: cfg.get_opt("snapshot_version"),
            dry_run: cfg.is_dry_run(),
            dry_run_to: cfg.get_opt("dry_run_to")
        })
    }

    /// no schema
    #[inline]
    fn find_set<'a>(&self, slf: &'a mut LoadResult, table: &String) -> Result<(String, String, String, &'a Table), String> {
        for s in slf.database.iter_mut() {
            if let Some(t) = s.tables.get_mut(table) {
                let dump_to = self.dump_to(format!("{}_{}.csv", s.schema_name, table))?;
                t.data_file = Some(dump_to.clone());
                return Ok((dump_to, s.schema_name.clone(), table.clone(), t));
            }
        }
        Err(tr!("etl.no_schema_table", table = table))
    }

    /// with schema
    #[inline]
    fn find_sets<'a>(&self, slf: &'a mut LoadResult, schema: &str, table: &String) -> Result<(String, String, String, &'a Table), String> {
        for s in slf.database.iter_mut() {
            if s.schema_name.as_str() == schema {
                return match s.tables.get_mut(&table) {
                    None => Err(tr!("etl.not_found", table = format!("{}.{}", schema, table))),
                    Some(t) => {
                        let dump_to = self.dump_to(format!("{}_{}.csv", s.schema_name, table))?;
                        t.data_file = Some(dump_to.clone());
                        Ok((dump_to, s.schema_name.clone(), table.clone(), t))
                    }
                }
            }
        }
        Err(tr!("etl.no_schema", table = format!("{}.{}", schema, table)))
    }
    // LoadResult

    /// same as  COPY ttest TO '/tmp/ttest_dn.csv' csv;
    pub(crate) async fn extract(&self, slf: &mut LoadResult, db_client: &mut Transaction<'_>, log: &Logger) -> Result<(), String> {
        for t in &self.tables.list {
            if t.table.len() > 0 {
                let (file, schema, table, yt) = match &t.schema {
                    None => self.find_set(slf, &t.table)?,
                    Some(s) => self.find_sets(slf, s.as_str(), &t.table)?
                };
                let mut write_header = true;
                let write_error = |e: csv::Error| tr!("etl.csv_write", file = file, err = e);
                let mut w = csv::Writer::from_path(file.as_str())
                    .map_err(|e| tr!("etl.csv_create", file = file, err = e))?;
                let mut c = 0;
                let source = t.source.clone().unwrap_or(format!("{}.{}", schema, table));
                let sql = format!("select * from {}", source);
                for r in db_client.query(sql.as_str(), &[]).await
                        .map_err(|e| tr!("etl.extract", source = source, err = crate::dbm_pg::pg(&e)))? {
                    if write_header {
                        for i in 0..r.len() {
                            let c = &r.columns()[i];
                            w.write_field(c.name()).map_err(write_error)?;
                        }
                        let _ = w.write_record(None::<&[u8]>).map_err(write_error)?;
                        write_header = false;
                    }
                    for i in 0..r.len() {
                        let _ = w.write_field(as_string(&r, i, yt)?).map_err(write_error)?;
                    }
                    let _ = w.write_record(None::<&[u8]>).map_err(write_error)?;
                    c += 1;
                }
                if c == 0 {
                    info!(log, "{}", tr!("etl.no_data", table = format!("{}.{}", schema, table), sql = sql, file = file));
                } else {
                    debug!(log, "{}", tr!("etl.extracted", count = c, file = file));
                }
                let _ = w.flush();
            }
        }
        Ok(())
    }

    #[inline]
    pub(crate) fn dump_loaded(&self, content: Result<String, String>) -> Result<(), String> {
        match content {
            Ok(loaded) => {
                match &self.schema_dump {
                    None => println!("{}", loaded),
                    Some(file) => {
                        if let Some(i) = file.rfind("/") {
                            let _ = fs::create_dir_all(&file[..i]).map_err(|e| tr!("etl.write_dir", file = file, err = e))?;
                        }

                        let file = if Path::new(&file).is_dir() {
                            self.dump_to(format!("{}.yaml", self.db_name))?
                        } else { file.clone() };

                        let _ = fs::write(&file, loaded).map_err(|e| tr!("etl.write", file = file, err = e))?;
                    },
                }
                Ok(())
            }
            Err(e) => Err(e)
        }
    }

    /// [superset] Somewhere to write the bundle: `--snapshot_to`, or a standard output that
    /// is no terminal - a ZIP is no text to show there. Asked before the schema is read
    #[cfg(feature = "superset")]
    pub(crate) fn superset_writable(&self) -> Result<(), String> {
        use std::io::IsTerminal;
        if self.schema_dump.is_none() && std::io::stdout().is_terminal() {
            return Err(tr!("etl.superset_tty"));
        }
        Ok(())
    }

    /// [superset] The snapshot as Apache Superset's import bundle, written where a snapshot
    /// is: to `--snapshot_to` - a file, or `D<n>__<db>.zip` in a directory - or to standard
    /// output, unless that is a terminal: a ZIP is no text to show there
    #[cfg(feature = "superset")]
    pub(crate) fn dump_superset(&self, loaded: &crate::dbm_log::LoadResult,
        db: &schema_guard_tokio::superset::SupersetDatabase) -> Result<(), String> {
        use schema_guard_tokio::superset;
        use std::io::Write;
        let files = superset::bundle(&superset::datasets_of_schemas(&loaded.database), db, std::time::SystemTime::now());
        let zip = superset::zip(&files, &format!("{}_superset", self.db_name))?;
        match &self.schema_dump {
            None => {
                let mut out = std::io::stdout();
                out.write_all(&zip).and_then(|_| out.flush()).map_err(|e| tr!("etl.write", file = "-", err = e))
            }
            Some(file) => {
                if let Some(i) = file.rfind("/") {
                    let _ = fs::create_dir_all(&file[..i]).map_err(|e| tr!("etl.write_dir", file = file, err = e))?;
                }
                let file = if Path::new(&file).is_dir() {
                    self.dump_to(format!("{}.zip", self.db_name))?
                } else { file.clone() };
                fs::write(&file, zip).map_err(|e| tr!("etl.write", file = file, err = e))
            }
        }
    }

    /// generate a NEXT file name ends with 'ftype', build dir.
    #[inline]
    fn dump_to(&self, ftype: String) -> Result<String, String> {
        let mut i = 0;
        let _ = fs::create_dir_all(&self.dir).map_err(|e| tr!("etl.read_dir", dir = self.dir, err = e));
        for path in fs::read_dir(&self.dir)
            .map_err(|e| tr!("etl.read_dir", dir = self.dir, err = e))? {

            let p = path.map_err(|e| tr!("etl.read_dir", dir = self.dir, err = e))?;
            let fi = p.file_name().to_str().unwrap_or("").to_string();
            if fi.len() > 1 && fi.as_bytes()[0] == b'D' && fi.ends_with(&ftype) {
                if let Some(pr) = fi.split("__").next() {
                    if let Ok(id) = pr[1..].to_string().parse::<i16>() {
                        if id > i {
                            i = id;
                        }
                    }
                }
            }
        }

        Ok(format!("{}{}D{}__{}", self.dir,
            if self.dir.as_bytes()[self.dir.len() - 1] == b'/' { "" } else { "/" },
            match &self.ver {
                None => format!("{}", i + 1),
                Some(v) => v.replace(".", "_")},
            ftype
        ))
    }

    #[inline]
    #[allow(dead_code)]
    pub(crate) fn is_anytable(&self) -> bool {
        self.tables.list.len() > 0 && self.tables.get(&"*".to_string()).is_some()
    }

    /// with schema
    #[inline]
    fn find(&self, schema: &String, table: &Table) -> bool {
        let tn = format!("{}.{}", schema, table.table_name);
        match self.tables.get(&tn) {
            None => match self.tables.get(&table.table_name) {
                None => false,
                Some(t) => !t.schema.is_some(),
            }
            Some(_t) => true
        }
    }
    // LoadResult

    pub(crate) async fn deploy_all(
        self,
        ysv: &OrderedHashMap<Schema>,
        logs: &LogSchemaType,
        dbc: &InfoSchemaType,
        db: &mut Transaction<'_>,
        lt: &mut Transaction<'_>,
        clog_t: &mut Option<Transaction<'_>>,
        stl: &StatementLog,
        log: &Logger,
    ) -> Result<bool, String> {
        let mut datas = DataTableMap::new();
        let fp = FileFilter::newt(".csv");
        let mut do_persist = false;
        for ys in &ysv.list {
            for yt in &ys.tables.list {
                // [sg-migrate] a template is never created, so there is no table to load
                if yt.is_template() {
                    continue;
                }
                if let Some(data_file) = &yt.data_file {
                    let file = format!("{}{}", self.dir, data_file);
                    let update = match self.rule {
                        EtlRule::Ignore => {
                            if !Path::new(file.as_str()).exists() {
                                warn!(log, "{}", tr!("etl.ignore_missing", table = yt.table_name, file = file));
                                return Ok(false);
                            }
                            false
                        }
                        EtlRule::Skip => {
                            if self.tables.len() == 0 {
                                info!(log, "{}", tr!("etl.skipped_all", file = file));
                                return Ok(false);
                            } else if self.find(&ys.schema_name, yt) {
                                info!(log, "{}", tr!("etl.skip_file", file = file, table = yt.table_name));
                                continue;
                            }
                            false
                        }
                        EtlRule::Clean | EtlRule::Force => {
                            if self.tables.len() == 0 {
                                true
                            } else {
                                let update = self.find(&ys.schema_name, yt);
                                if update {
                                    debug!(log, "{}", tr!("etl.rule_set", rule = self.rule, table = yt.table_name, file = file));
                                } else {
                                    info!(log, "{}", tr!("etl.rule_not_set", rule = self.rule, table = yt.table_name, file = file));
                                }
                                update
                            }
                        },
                        EtlRule::Apply => false
                    };
                    let readable = { File::open(&file).map_err(|e| tr!("etl.cannot_read", file = file, err = e))?; true };
                    if readable {
                        let data_file_ver = DataFileVersion::new(data_file, &fp, Some(yt));
                        let dstl = stl.instance('D', &data_file_ver.v.s, &self.dir, data_file);
                        {
                            if let Some((_sl_file, checks)) = logs.get(&data_file_ver.file) {
                                if checks == &dstl.checksum {
                                    info!(log, "{}", tr!("etl.unchanged", file = file, table = yt.table_name, key = data_file_ver.file));
                                    continue;
                                }
                            };
                        }
                        datas.insert(data_file_ver, (&ys.schema_name, yt, update, file, dstl));
                    } else {
                        return Err(tr!("etl.not_readable", file = data_file, table = yt.table_name));
                    }


                }
            }
        }

        if datas.len() > 0 {
            debug!(log, "{}", tr!("etl.uploading_count", count = datas.len()));
        }

        for (dfv, (schema_name, yt, update, file, dstl)) in datas.iter() {
            debug!(log, "{}", tr!("etl.uploading", file = file));
            crate::dbm_digest::source('D', &dfv.v.s, file.strip_prefix(self.dir.as_str()).unwrap_or(file), None);
            let mut r = csv::Reader::from_path(file)
                .map_err(|e| tr!("etl.read_csv", file = file, err = e))?;
            match EtlComposer::new(&self, dbc, *schema_name, *yt, *update, file, &mut r, log)?
                .deploy(&mut r, db).await {
                Ok(ok) => {
                    if ok {
                        let _ = dstl.persist(None, Some(lt), clog_t).await?;
                        do_persist = !self.dry_run;
                        if self.table_tr && !self.dry_run {
                            if let Err(ec) = db.batch_execute("COMMIT; BEGIN").await {
                                let _ = dstl.persist(Some(crate::dbm_pg::pg(&ec)), Some(lt), clog_t).await?;
                                return Err(tr!("etl.commit", table = format!("{}.{}", schema_name, yt.table_name), err = crate::dbm_pg::pg(&ec)));
                            }
                        }
                    } else {
                        info!(log, "{}", tr!("etl.nothing_loaded", file = file, table = yt.table_name));
                    }
                }
                Err(err) => {
                    let _ = dstl.persist(Some(err.clone()), Some(lt), clog_t).await?;
                    return Err(err.to_owned());
                }
            }
        }

        Ok(do_persist)
    }

}

#[derive(Debug, Clone)]
struct DataFileVersion {
    file: String,
    v: FileVersion,
    w: u8,
}

impl DataFileVersion {
    // TODO: a unit test
    fn new(data_file: &String, fp: &FileFilter, yto: Option<&Table>) -> Self {
        let x = match data_file.rfind("/") {
            None => 0,
            Some(x) => x + 1
        };
        let zerov = (Version::new(0, 0, 0), "".to_string());
        let (vv, key) = match fp.parse_type(data_file, x) {
            Ok(f) => {
                match f {
                    None =>  zerov.clone(),
                    Some(v) => v
                }
            }
            Err(e) => {
                println!("{}", e);
                zerov.clone()
            }
        };
        // let (vv, key) = fp.parse_type(data_file, x + 1)
        //     .unwrap_or(Some(zerov.clone())).unwrap_or(zerov.clone());
        DataFileVersion {
            file: data_file[x..].to_string(),
            v: FileVersion::new(vv, key),
           w: match yto {
               None => 0,
               Some(yt) => pseudo_weight(yt)
           }
        }
    }

    fn cmp(&self, other: &Self)  -> Ordering {
        match self.v.cmp(&other.v) {
            Ordering::Equal => {
                if self.w > other.w {
                    Ordering::Greater
                } else if self.w < other.w{
                    Ordering::Less
                } else {
                    self.file.cmp(&other.file)
                }
            }
            r @ _ => r
        }
    }
}

pub fn pseudo_weight(yself: &Table) -> u8 {
    let mut w = 0;
    for c in &yself.columns.list {
        if let Some(x) = &c.constraint {
            if x.foreign_key.is_some() {
                w += 1;
            }
        }
    }
    w
}

impl Eq for DataFileVersion {}

impl PartialEq for DataFileVersion {
    fn eq(&self, other: &Self) -> bool {
        self.w == other.w && self.file.eq(&other.file) && self.v.eq(&other.v)
    }
}

impl PartialOrd for DataFileVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(&other))
    }
}

impl Ord for DataFileVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.cmp(&other)
    }
}

type DataTableMap<'a> = BTreeMap<DataFileVersion, (&'a String, &'a Table, bool, String, StatementLog)>;

type ColumnSet = BTreeSet<u16>;

/// Helper bundle to compose insert statements
#[derive(Debug)]
struct EtlComposer<'a> {
    /// execution context
    etl: &'a Etl,
    /// PK columns default for sequence build
    pk_defaults: BTreeMap<String, String>,
    /// list of PK from file header: indexes from header
    /// PK list of columns, might be empty, but usually one name
    hidx_pks_clmn: ColumnSet,
    /// list of nullable columns
    hidx_null_clmn: ColumnSet,
    /// list of nullable (VAR)CHAR/TEXT columns to apply NULL unescape
    hidx_null_str: ColumnSet,
    /// list of (VAR)CHAR/TEXT columns to escape single quotes
    hidx_str_clmn: ColumnSet,
    /// ON CONFLICT (pks) DO UPDATE
    update: bool,
    /// list of columns from CSV file
    headers: StringRecord,
    /// comma separated list of columns
    columns_lst: String,
    /// comma separated list of PK columns
    columns_pklst: String,
    /// schema.table
    table: String,
    /// schema
    schema: String,
    /// for logging
    file: String,
    log: Logger,
}

const IS_NULL:&str = " IS NULL";
const SETE_NULL:&str = " = NULL";

fn add_sequence(
    pk_defaults: &mut BTreeMap<String, String>,
    schema_name: &String,
    yaml_table: &Table,
    column_name: &String,
    column_type: &String,
    column_default: &Option<String>
) {
    if !pk_defaults.contains_key(column_name) {
        if column_type.ends_with("serial") {
            pk_defaults.insert(column_name.clone(), format!("nextval('{}.{}_{}_seq')",
                schema_name, yaml_table.table_name, column_name));
        } else {
            if let Some(def) = column_default {
                pk_defaults.insert(column_name.clone(), def.clone());
            }
        }
    }
}

impl <'a> EtlComposer <'a>  {
    /// prepare composer context
    #[inline]
    fn new(
        etl_cmd: &'a Etl,
        db_info_schema: &'a InfoSchemaType,
        schema_name: &'a String,
        yaml_table: &'a Table,
        do_force_update: bool,
        src_file: &String, // to use for logging only
        csv_reader: &mut Reader<File>,
        log: &'a Logger,
    ) -> Result<Self, String>
    {
        let headers = csv_reader.headers().map_err(|e| tr!("etl.read_csv", file = src_file, err = e))?.clone();
        let mut pks  = BTreeSet::new();
        let mut null_clmn  = BTreeSet::new();
        let mut null_str = BTreeSet::new();
        let mut str_clmn = BTreeSet::new();
        let mut pk_defaults = BTreeMap::new();

        if let Some(dbcs) =  db_info_schema.get(schema_name) {
            if let Some(dbt) = dbcs.get(&yaml_table.table_name) {
                for (cn, cd) in &dbt.columns {
                    if cd.pk {
                        pks.insert(cn.as_str());
                        add_sequence(&mut pk_defaults, schema_name, yaml_table, cn, &cd.column_type, &cd.column_default);
                    }
                    if cd.nullable{
                        null_clmn.insert(cn.as_str());
                        if is_str_escapable(&cd.column_type) {
                            null_str.insert(cn.as_str());
                        }
                    }
                    if is_str_escapable(&cd.column_type) {
                        str_clmn.insert(cn.as_str());
                    }

                }
            }
        }

        for c in &yaml_table.columns.list {
            let nullable = match &c.constraint {
                Some(pk) => {
                    if pk.primary_key.unwrap_or(false) {
                        pks.insert(c.name.as_str());
                        add_sequence(&mut pk_defaults, schema_name, yaml_table, &c.name, &c.column_type, &c.default_value);
                    }
                    pk.nullable
                }
                None => true,
            };
            if nullable {
                null_clmn.insert(c.name.as_str());
                if is_str_escapable(&c.column_type) {
                    null_str.insert(c.name.as_str());
                }
            }
            if is_str_escapable(&c.column_type) {
                str_clmn.insert(c.name.as_str());
            }
        }

        let table = format!("{}.{}", schema_name, yaml_table.table_name);

        let mut hidx_null_clmn: ColumnSet = Default::default();
        let mut hidx_null_str: ColumnSet = Default::default();
        let mut hidx_str_clmn: ColumnSet = Default::default();
        let mut hidx_pks_clmn: ColumnSet = Default::default();
        let mut columns_lst = String::new();
        let mut columns_pklst = String::new();
        for i in 0..headers.len() {
            let cn = headers[i].to_string();
            if pks.contains(cn.as_str()) {
                hidx_pks_clmn.insert(i as u16);
                if columns_pklst.len() > 0 {
                    columns_pklst.push_str(", ");
                }
                columns_pklst.push_str(cn.as_str());
            }
            if i > 0 {
                columns_lst.push_str(", ");
            }
            columns_lst.push_str(cn.as_str());
            if null_clmn.contains(cn.as_str()) {
                hidx_null_clmn.insert(i as u16);
            }
            if null_str.contains(cn.as_str()) {
                hidx_null_str.insert(i as u16);
            }
            if str_clmn.contains(cn.as_str()) {
                hidx_str_clmn.insert(i as u16);
            }
        }
        let etl = EtlComposer {
            etl: etl_cmd,
            pk_defaults,
            hidx_pks_clmn,
            hidx_null_clmn,
            hidx_null_str,
            hidx_str_clmn,
            update: do_force_update && etl_cmd.tables.get(&table).is_some(),
            headers,
            columns_lst,
            columns_pklst,
            table,
            schema: schema_name.clone(),
            file: src_file.clone(), // to use for logging
            log: log.clone()
        };
        // #[cfg(test)]
        #[cfg(debug_assertions)]
        {
            let mut dbc = db_info_schema.clone();
            dbc.remove("public");
            debug!(log, "DEBUG_ETL \n{:?}\n{:?}\n{:?}\n PKS: {:?} \n null_clmn: {:?}\n null_str: {:?}\n str_clmn: {:?}\n   ",
                yaml_table.columns.list, dbc, etl, pks, null_clmn, null_str, str_clmn);
        }
       Ok(etl)
    }

    #[inline]
    async fn deploy(&self, csv_reader: &mut Reader<File>, db: &mut Transaction<'_>) -> Result<bool, String>
    {
        let mut cnt = 0;
        let mut rows = 0;   // the records read: a dry run's statements, which it runs none of
        let mut clean_sql = String::new();
        // [plan] each row's key - with no key, its every column - for a plan's counts
        let mut keys = vec![];
        let mut record = csv::StringRecord::new();
        let clean = match self.etl.rule {
            EtlRule::Clean => self.update, // clean only if update
            _ => false
        };
        info!(self.log, "{}", tr!("etl.loading", file = self.file, table = self.table, rule = self.etl.rule));
        while csv_reader.read_record(&mut record)
            .map_err(|e| tr!("etl.read_csv", file = self.file, err = e))?
        {
            let sql = if self.hidx_pks_clmn.len() == 0 {
                if clean {
                    warn!(self.log, "{}", tr!("etl.clean_no_pk", table = self.table));
                }
                if self.etl.dry_run {
                    keys.push(self.conditions_all(&record));
                }
                self.insert_statement_nopk(&record)
            } else {
                if clean {
                    self.values_inpk(&record, &mut clean_sql);
                }
                if self.etl.dry_run {
                    let mut k = String::new();
                    self.values_inpk(&record, &mut k);
                    keys.push(k);
                }
                self.insert_statement_pk(&record)
            };

            rows += 1;
            cnt += self.execute("insert/update", sql, db).await?;
        }
        if self.etl.dry_run {
            self.plan(&keys, &clean_sql);
        }
        let cleaned = if clean_sql.len() > 0 {
            let (lb, rb) = if self.hidx_pks_clmn.len() > 1 { ('(',')') } else { (' ', ' ')};
            self.execute("clean update", format!("DELETE FROM {} WHERE {}{}{} NOT IN ({})", self.table, lb, self.columns_pklst, rb, clean_sql), db).await?
        } else { 0 };
        if cleaned > 0 {
            info!(self.log, "{}", tr!("etl.cleaned", count = cleaned, file = self.file, table = self.table));
        }
        // the sequences after the rows: in a dry run's plan too, which counted none
        if cnt > 0 || (self.etl.dry_run && rows > 0) {
            for (pk, def) in &self.pk_defaults {
                if let Some(sql) = self.build_sequence(pk, def) {
                    let _ = self.execute("sequence",sql, db).await?;
                }
            }
        }
        if cnt > 0 {
            info!(self.log, "{}", tr!("etl.loaded", count = cnt, file = self.file, table = self.table));
        }
        Ok(cleaned + cnt > 0)
    }

    async fn execute(&self, method: &str, sql: String, db: &mut Transaction<'_>) -> Result<u64, String> {
        if self.etl.dry_run {
            // [plan] the rows - and the sequences set after them - counted once the file
            // is read (plan)
            let _ = StatementLog::dry_store(self.etl.dry_run_to.clone(), format!("{};\n", sql), &self.log)?;
            Ok(0)
        } else {
            #[cfg(debug_assertions)] debug!(self.log, "ETL_{}: {}", method, sql);
            db.execute(sql.as_str(), &[]).await
                .map_err(|e| tr!("etl.execute", method = method, table = self.table, err = crate::dbm_pg::pg(&e)))
        }
    }

    /// [plan] the file's rows as a plan counts them: those the table has already - by key,
    /// so many at a time; with no key, those it has a row equal to - inserted less those,
    /// and updated when the rule updates; those the clean deletes; the sequences set after,
    /// when a row loads. A row met twice, once
    fn plan(&self, keys: &[String], clean_sql: &str) {
        let unique: Vec<&String> = keys.iter().collect::<BTreeSet<_>>().into_iter().collect();
        let pk = !self.hidx_pks_clmn.is_empty();
        let (lb, rb) = if self.hidx_pks_clmn.len() > 1 { ("(", ")") } else { ("", "") };
        let matched = if pk {
            unique.chunks(crate::dbm_digest::KEYS).map(|ch| format!("SELECT count(*) FROM {} WHERE {}{}{} IN ({})",
                self.table, lb, self.columns_pklst, rb, ch.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", "))).collect()
        } else {
            unique.chunks(100).map(|ch| format!("SELECT {}", ch.iter()
                .map(|c| format!("(EXISTS (SELECT 1 FROM {} WHERE {}))::int", self.table, c)).collect::<Vec<_>>().join(" + ")))
                .collect()
        };
        let deleted = if clean_sql.is_empty() {
            vec![]
        } else {
            vec![format!("SELECT count(*) FROM {} WHERE {}{}{} NOT IN ({})", self.table, lb, self.columns_pklst, rb, clean_sql)]
        };
        let setval = self.pk_defaults.iter().any(|(pk, def)| self.build_sequence(pk, def).is_some());
        crate::dbm_digest::data(&self.table, unique.len() as u64, matched, pk && self.update, deleted, setval);
    }

    /// bump sequence from default
    #[inline]
    fn build_sequence(&self, pk: &String, def: &String) -> Option<String> {
        if def.starts_with("nextval") {
            let names:Vec<&str> = def.split("'").collect();
            let sequence = if names[1].contains(".") {
                names[1].to_string()
            } else {
                format!("{}.{}", self.schema, names[1])
            };
            if names.len() > 2 {
                return Some( format!("SELECT setval('{}', coalesce(max({}), 0)) + 1 FROM {}",
                    sequence, pk, self.table));
            }
        }
        None
    }

    #[inline]
    fn conditions_all(&self, record: &StringRecord) -> String {
        let mut sql = String::new();
        for i in 0..self.headers.len() {
            self.build(record, &mut sql, i as u16, true, true);
        }
        sql
    }

    #[inline]
    #[allow(unused)]
    fn conditions_pk(&self, record: &StringRecord) -> String {
        let mut sql = String::new();
        for i in &self.hidx_pks_clmn {
            self.build(record, &mut sql, *i as u16, true, true);
        }
        sql
    }

    /// is_null: true:  'is null' 'and'; false: '=null' ','
    #[inline]
    fn build(&self, record: &StringRecord, sql: &mut String, idx: u16, with_field: bool, is_null: bool) {
        if sql.len() > 0 {
            sql.push_str(if is_null {" AND "} else {", "} );
        }
        if with_field {
            sql.push_str(&self.headers[idx as usize]);
        }
        if self.hidx_null_str.contains(&idx) {
            match unesc_null(record[idx as usize].to_string()) {
                None => {
                    sql.push_str(if is_null { IS_NULL } else if with_field { SETE_NULL } else { NULL });
                },
                Some(v) => {
                    escp_append(sql, v.as_str(), with_field);
                }
            }
        } else if self.hidx_str_clmn.contains(&idx) {
            escp_append(sql, &record[idx as usize], with_field);
        } else if self.hidx_null_clmn.contains(&idx) && record[idx as usize].len() == 0 {
            sql.push_str(if is_null { IS_NULL } else if with_field { SETE_NULL } else { NULL });
        } else {
            if with_field {
                sql.push('=');
            }
            sql.push_str(&record[idx as usize]);
        }
    }

    #[inline]
    fn values_all(&self, record: &StringRecord) -> String {
        let mut sql = String::new();
        for i in 0..self.headers.len() {
            self.build(record, &mut sql, i as u16, false, false);
        }
        sql
    }

    #[inline]
    fn values_nopk(&self, record: &StringRecord) -> String {
        let mut sql = String::new();
        for i in 0..self.headers.len() {
            let idx = i as u16;
            if !self.hidx_pks_clmn.contains(&idx) {
                self.build(record, &mut sql, idx, true, false);
            }
        }
        sql
    }

    #[inline]
    fn values_inpk(&self, record: &StringRecord, x: &mut String) {
        let mut sql = String::new();
        for i in &self.hidx_pks_clmn {
            self.build(record, &mut sql, *i as u16, false, false);
        }
        if self.hidx_pks_clmn.len() > 1 {
            sql.insert(0, '(');
            sql.push(')');
        }
        if x.len() > 0 {
            x.push_str(", ");
        }
        x.push_str(sql.as_str());
    }

/// Extracted to method for testing purposes
    #[inline]
    fn insert_statement_nopk(&self, record: &StringRecord) -> String {
        format!("INSERT INTO {} ({}) SELECT {} WHERE (SELECT COUNT(*) FROM {} WHERE {}) = 0",
            self.table,
            self.columns_lst,
            self.values_all(record),
            self.table,
            self.conditions_all(record)
        )
    }

/// Extracted to method for testing purposes
    #[inline]
    fn insert_statement_pk(&self, record: &StringRecord) -> String {
        format!("INSERT INTO {} ({}) VALUES ({}) ON CONFLICT ({}) DO {} {}",
            self.table,
            self.columns_lst,
            self.values_all(record),
            self.columns_pklst,
            if self.update {
                "UPDATE SET"
            } else {
                "NOTHING"
            },
            if self.update {
                self.values_nopk(record)
            } else {
                "".to_string()
            }
        )
    }

}
//
#[inline]
fn escp_append(sql: &mut String, input: &str, with_field: bool){
    let input = input.replace("'", "''");
    if with_field {
        sql.push_str("=");
    }
    sql.push_str("'");
    sql.push_str(input.as_str());
    sql.push_str("'");
}


#[cfg(test)]
#[allow(unused, warnings)]
mod tests {
    use csv::ReaderBuilder;
    use schema_guard_tokio::column::Column;
    use sloggers::Build;
    use sloggers::terminal::*;
    use sloggers::types::*;
    use uuid::Uuid;
    use super::*;

    fn logger(debug: bool) -> Logger {
        let mut builder = TerminalLoggerBuilder::new();
        builder.channel_size(1024);
        builder.destination(Destination::Stdout);
        builder.format(Format::Compact);
        builder.source_location(SourceLocation::None);
        builder.level(if debug {
            Severity::Debug
        } else {
            Severity::Info
        });
        builder.build().unwrap()
    }


    #[test]
    fn s_test() {
        let tv:Vec<&str> = "t".split(":").collect();
        assert_eq!(tv.len(), 1);
        assert_eq!("t", "t".split(":").next().unwrap_or(""));
    }

    #[test]
    fn c_test() {
            let data = "\
city,country,pop
Boston,United States,\"4628910\"\"\"
";
    let mut rdr = ReaderBuilder::new()
        .delimiter(b',')
        .from_reader(data.as_bytes());

        if let Some(result) = rdr.records().next() {
            let record = result.unwrap();
            assert_eq!(record, vec!["Boston", "United States", "4628910\""]);
        } else {
            assert!(false);
        }

    }

    #[test]
    fn h_test() {
            let data = "\
Boston,United States,\"4628910\"\"\"
";
        let mut rdr = ReaderBuilder::new()
            .has_headers(false)
            .delimiter(b',')
            .from_reader(data.as_bytes());

        assert!(!rdr.has_headers());

        if let Some(result) = rdr.records().next() {
            let record = result.unwrap();
            assert_eq!(record, vec!["Boston", "United States", "4628910\""]);
        } else {
            assert!(false);
        }
    }

    fn etl_test() -> Etl {
        let mut tables = OrderedHashMap::new();
        tables.append(EtlTable{
            schema: Some("public".to_string()),
            table: "rdict".to_string(),
            source: None
        });
        Etl {
            tables,
            rule: EtlRule::Apply,
            db_name: "".to_string(),
            dir: "./".to_string(),
            schema_dump: None,
            table_tr: false,
            yaml_format: false,
            superset: false,
            ver: None,
            dry_run: false,
            dry_run_to: None
        }
    }

    fn yt_test() -> Table {
        let mut columns = OrderedHashMap::new();
        columns.append(Column::newt("r", "serial", true, false));
        columns.append(Column::newt("active", "", false, true));
        columns.append(Column::newt("starts_at", "date", false, true));
        columns.append(Column::newt("ends_at", "date", false, true));
        columns.append(Column::newt("cap", "varchar", false, true));
        Table {
            // [sg-tokio] a plain table, not a template
            is_template: None,
            use_templates: vec![],
            table_name: "rdict".to_string(),
            description: "".to_string(),
            transaction: "".to_string(),
            sql: "".to_string(),
            constraint: "".to_string(),
            columns,
            triggers: OrderedHashMap::new(),
            data_file: None,
            owner: "".to_string(),
            data: vec![],
            grant: vec![],
        }
    }

    #[inline]
    fn uuid() -> String {
        let x = Uuid::new_v4();
        x.hyphenated().to_string()[..8].into()
    }

    #[test]
    fn w_test() {
        let log = logger(true);
        #[cfg(feature = "tls")]
        let file = "/tmp";
        #[cfg(not(feature = "tls"))]
        let file = "./target";
        let file = format!("{}/{}.csv", file, uuid());
        let data = "r,active,starts_at,ends_at,cap\n\
1,true,,,NULL\n\
2,true,,,\\NULL\n\
3,true,,'2020-01-01','NULL'\n\
";
        fs::write(&file, data);
        let mut rdr = ReaderBuilder::new()
            .from_path(&file).unwrap();
        let etl = etl_test();
        let yt = yt_test();
        let dbc = Default::default();
        let schema = "public".to_string();
        // let schema = "".to_string();
        let ec_test = EtlComposer::new(&etl, &dbc, &schema, &yt, true, &file, &mut rdr, &log).unwrap();
        println!("{:?}", ec_test);
        assert_eq!(ec_test.hidx_pks_clmn.len(), 1);
        assert_eq!(ec_test.headers.len(), yt.columns.len());
        assert_eq!(ec_test.hidx_pks_clmn.iter().next().unwrap(), &0u16);
        assert_eq!(ec_test.hidx_null_clmn.len(), yt.columns.len() - 1);
        assert_eq!(ec_test.hidx_null_str.len(), 1);
        assert_eq!(ec_test.hidx_str_clmn.len(), 1);
        assert_eq!(ec_test.hidx_null_str.iter().next().unwrap(), &4u16);
        assert_eq!(ec_test.hidx_str_clmn.iter().next().unwrap(), &4u16);
        assert_eq!(ec_test.columns_lst.as_str(), "r, active, starts_at, ends_at, cap");
        assert_eq!(ec_test.columns_pklst.as_str(), "r");

        let mut record = csv::StringRecord::new();
        assert!(rdr.read_record(&mut record).unwrap());
        assert_eq!(ec_test.conditions_all(&record), "r=1 AND active=true AND starts_at IS NULL AND ends_at IS NULL AND cap IS NULL");
        assert_eq!(ec_test.conditions_pk(&record), "r=1");
        assert_eq!(ec_test.insert_statement_pk(&record), "INSERT INTO public.rdict (r, active, starts_at, ends_at, cap) VALUES (1, true, NULL, NULL, NULL) \
            ON CONFLICT (r) DO UPDATE SET active=true, starts_at = NULL, ends_at = NULL, cap = NULL");
        assert_eq!(ec_test.insert_statement_nopk(&record), "INSERT INTO public.rdict (r, active, starts_at, ends_at, cap) \
            SELECT 1, true, NULL, NULL, NULL WHERE (SELECT COUNT(*) FROM public.rdict \
                    WHERE r=1 AND active=true AND starts_at IS NULL AND ends_at IS NULL AND cap IS NULL) = 0");

        assert!(rdr.read_record(&mut record).unwrap());
        assert_eq!(ec_test.conditions_all(&record), "r=2 AND active=true AND starts_at IS NULL AND ends_at IS NULL AND cap='NULL'");
        assert_eq!(ec_test.conditions_pk(&record), "r=2");
        assert_eq!(ec_test.insert_statement_pk(&record), "INSERT INTO public.rdict (r, active, starts_at, ends_at, cap) VALUES (2, true, NULL, NULL, 'NULL') \
            ON CONFLICT (r) DO UPDATE SET active=true, starts_at = NULL, ends_at = NULL, cap='NULL'");

        assert!(rdr.read_record(&mut record).unwrap());
        assert_eq!(ec_test.conditions_all(&record), "r=3 AND active=true AND starts_at IS NULL AND ends_at='2020-01-01' AND cap='''NULL'''");
        assert_eq!(ec_test.conditions_pk(&record), "r=3");
        assert_eq!(ec_test.insert_statement_pk(&record), "INSERT INTO public.rdict (r, active, starts_at, ends_at, cap) VALUES (3, true, NULL, '2020-01-01', '''NULL''') \
            ON CONFLICT (r) DO UPDATE SET active=true, starts_at = NULL, ends_at='2020-01-01', cap='''NULL'''");

        fs::remove_file(&file);
    }


    #[test]
    fn d_test() {
        let ff = FileFilter::newt(".csv");
        let df = DataFileVersion::new(&"D1__x.csv".to_string(), &ff, None);
        assert_eq!(df.file.as_str(), "D1__x.csv");
        let df = DataFileVersion::new(&"D1_1__x.csv".to_string(), &ff, None);
        assert_eq!(df.file.as_str(), "D1_1__x.csv");
        let df = DataFileVersion::new(&"D1_1_1__x.csv".to_string(), &ff, None);
        assert_eq!(df.file.as_str(), "D1_1_1__x.csv");
        let df = DataFileVersion::new(&"x/D1__x.csv".to_string(), &ff, None);
        assert_eq!(df.file.as_str(), "D1__x.csv");

    }

    /// a scratch directory of the test's own, with `/` at the end as Etl's dir has it
    fn scratch(name: &str) -> String {
        let d = std::env::temp_dir().join(format!("sge-etl-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        format!("{}/", d.display())
    }

    #[test]
    fn numbers_are_written_bare_and_the_rest_quoted() {
        for t in [Type::INT2, Type::INT4, Type::INT8, Type::FLOAT4, Type::FLOAT8] {
            assert!(!is_quoted(&t), "{}", t);
        }
        for t in [Type::TEXT, Type::DATE, Type::BOOL] {
            assert!(is_quoted(&t), "{}", t);
        }
    }

    #[test]
    fn a_rule_reads_and_prints_as_written() {
        for r in ["clean", "force", "apply", "ignore", "skip"] {
            assert_eq!(EtlRule::from(r.to_uppercase()).to_string(), r);
        }
        assert_eq!(EtlRule::from("anything".to_string()).to_string(), "apply");
    }

    /// [etl] snapshot_data names the tables to extract - schema-qualified or not, a view
    /// of its own after a colon; snapshot_to is a directory, or a file in one
    #[test]
    fn the_tables_and_where_their_data_goes_read_from_the_configuration() {
        let dir = scratch("from");
        let to = dir.trim_end_matches('/').to_string();
        let e = Etl::from(&ConfigMap::of(&[("snapshot_data", "app.users,orders:app.open_orders"), ("snapshot_to", &to)])).unwrap();
        let tables: Vec<(String, Option<String>)> = e.tables.list.iter().map(|t| (t.get_name(), t.source.clone())).collect();
        assert_eq!(tables, [("app.users".to_string(), None), ("orders".to_string(), Some("app.open_orders".to_string()))]);
        assert_eq!((e.dir.as_str(), e.schema_dump.as_deref()), (dir.as_str(), Some(to.as_str())));
        assert_eq!(Etl::from(&ConfigMap::of(&[("snapshot_to", "snap.yaml")])).unwrap().dir, "./");
        let e = Etl::from(&ConfigMap::of(&[("snapshot_to", "out/snap.yaml"), ("out_format", "JSON"), ("etl", "force"),
            ("schema_transaction", "table"), ("snapshot_version", "1.2"), ("dry_run", "true")])).unwrap();
        assert_eq!(e.dir, "out/");
        assert!(!e.yaml_format && e.table_tr && e.dry_run);
        assert_eq!((e.rule.to_string().as_str(), e.ver.as_deref()), ("force", Some("1.2")));
        let e = Etl::from(&ConfigMap::of(&[("source_dir", "/scripts/")])).unwrap();
        assert_eq!(e.dir, "/scripts/");
        assert!(e.tables.list.is_empty() && e.schema_dump.is_none());
        let _ = fs::remove_dir_all(dir);
    }

    /// [etl] a snapshot file is numbered after the last of its kind there, or by
    /// snapshot_version; the snapshot goes to its file, or into the directory named
    #[test]
    fn a_snapshot_file_is_numbered_after_the_last() {
        let dir = scratch("dump");
        let mut e = etl_test();
        e.dir = dir.clone();
        e.db_name = "app".into();
        assert_eq!(e.dump_to("t.csv".into()).unwrap(), format!("{}D1__t.csv", dir));
        for f in ["D7__t.csv", "Dx__t.csv", "D9__other.csv"] {
            fs::write(format!("{}{}", dir, f), "").unwrap();
        }
        assert_eq!(e.dump_to("t.csv".into()).unwrap(), format!("{}D8__t.csv", dir));
        e.ver = Some("1.2".into());
        assert_eq!(e.dump_to("t.csv".into()).unwrap(), format!("{}D1_2__t.csv", dir));
        e.ver = None;

        e.schema_dump = Some(format!("{}sub/snap.yaml", dir));
        e.dump_loaded(Ok("database: []\n".into())).unwrap();
        assert_eq!(fs::read_to_string(format!("{}sub/snap.yaml", dir)).unwrap(), "database: []\n");
        e.schema_dump = Some(dir.trim_end_matches('/').to_string());
        e.dump_loaded(Ok("x".into())).unwrap();
        assert_eq!(fs::read_to_string(format!("{}D1__app.yaml", dir)).unwrap(), "x", "a directory: the database's name");
        assert_eq!(e.dump_loaded(Err("e".into())), Err("e".to_string()));
        e.schema_dump = None;
        e.dump_loaded(Ok("to stdout".into())).unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    fn schema(name: &str, tables: Vec<Table>) -> Schema {
        let mut t = OrderedHashMap::new();
        for x in tables {
            t.append(x);
        }
        Schema { schema_name: name.into(), owner: "".into(), tables: t, file: "".into() }
    }

    /// [etl] a table to extract is found by its name in any schema, or in the one named;
    /// a table to load is matched against the list the same way
    #[test]
    fn a_table_is_found_in_its_schema() {
        let dir = scratch("find");
        let mut e = etl_test();
        e.dir = dir.clone();
        let mut lr = LoadResult { database: vec![schema("app", vec![yt_test()])] };
        let (file, s, t, _) = e.find_set(&mut lr, &"rdict".to_string()).unwrap();
        assert_eq!((file.clone(), s.as_str(), t.as_str()), (format!("{}D1__app_rdict.csv", dir), "app", "rdict"));
        assert_eq!(lr.database[0].tables.get(&"rdict".to_string()).unwrap().data_file, Some(file));
        let (_, s, _, _) = e.find_sets(&mut lr, "app", &"rdict".to_string()).unwrap();
        assert_eq!(s, "app");
        assert!(e.find_sets(&mut lr, "app", &"nope".to_string()).unwrap_err().starts_with("SGE-E404 "));
        assert!(e.find_sets(&mut lr, "nope", &"rdict".to_string()).unwrap_err().starts_with("SGE-E405 "));
        assert!(e.find_set(&mut lr, &"nope".to_string()).unwrap_err().starts_with("SGE-E403 "));

        // etl_test lists public.rdict
        let yt = yt_test();
        assert!(e.find(&"public".to_string(), &yt));
        assert!(!e.find(&"app".to_string(), &yt), "another schema's table of that name");
        assert!(!e.is_anytable());
        e.tables.append(EtlTable { schema: None, table: "rdict".into(), source: None });
        assert!(e.find(&"app".to_string(), &yt), "listed with no schema: in any");
        e.tables.append(EtlTable { schema: None, table: "*".into(), source: None });
        assert!(e.is_anytable());
        let _ = fs::remove_dir_all(dir);
    }

    /// [etl] data files load by version, then the tables with fewer foreign keys first -
    /// the tables they refer to are loaded by then - then by name
    #[test]
    fn data_files_load_by_version_then_by_their_foreign_keys() {
        use schema_guard_tokio::column::{Constr, ForeignKey};
        let ff = FileFilter::newt(".csv");
        let plain = yt_test();
        let mut referring = yt_test();
        let mut fk = Column::newt("ref", "int", false, true);
        fk.constraint = Some(Constr { primary_key: None, nullable: true,
            foreign_key: Some(ForeignKey { references: "other".into(), sql: "".into() }) });
        referring.columns.append(fk);
        assert_eq!((pseudo_weight(&plain), pseudo_weight(&referring)), (0, 1));
        let v = |f: &str, t: Option<&Table>| DataFileVersion::new(&f.to_string(), &ff, t);
        let mut files = vec![v("D2__a.csv", None), v("D1__b.csv", Some(&referring)), v("D1__c.csv", Some(&plain)),
            v("D1__a.csv", Some(&plain))];
        files.sort();
        let names: Vec<&str> = files.iter().map(|f| f.file.as_str()).collect();
        assert_eq!(names, ["D1__a.csv", "D1__c.csv", "D1__b.csv", "D2__a.csv"]);
        assert!(files[0] < files[1] && files[2] > files[1]);
        assert!(v("D1__a.csv", None) == v("x/D1__a.csv", None));
        assert!(v("D1__a.csv", None) != v("D1__a.csv", Some(&referring)));
        // a name with no version: version 0
        assert_eq!(v("Dx.csv", None).v, FileVersion::new(Version::new(0, 0, 0), "".into()));
    }

    /// [etl] the database's own columns shape the statements with the file's: its
    /// primary keys - the default to bump a sequence from - its text columns, quoted,
    /// and those nullable
    #[test]
    fn the_databases_columns_shape_the_statements() {
        use schema_guard_tokio::loader::{PgColumnDfn, PgTable};
        let log = logger(false);
        let file = format!("./target/{}.csv", uuid());
        fs::write(&file, "id,code,name,note\n1,A,O'Brien,\n2,B,x,NULL\n").unwrap();
        let mut rdr = ReaderBuilder::new().from_path(&file).unwrap();
        let col = |name: &str, t: &str, pk: bool, nullable: bool, def: Option<&str>| PgColumnDfn {
            column_name: name.into(), column_type: t.into(), column_default: def.map(|d| d.into()), sql: None,
            fk: None, pk, nullable, sort_order: 0, column_comment: None };
        let mut t = PgTable::default();
        t.table_name = "items".into();
        for c in [col("id", "integer", true, false, Some("nextval('items_id_seq'::regclass)")),
            col("code", "character varying", true, false, None), col("name", "character varying", false, false, None),
            col("note", "text", false, true, None)] {
            t.columns.insert(c.column_name.clone(), c);
        }
        let mut dbc: InfoSchemaType = Default::default();
        dbc.insert("app".into(), vec![("items".to_string(), t)].into_iter().collect());
        let mut yt = yt_test();
        yt.table_name = "items".into();
        yt.columns = OrderedHashMap::new();
        let mut etl = etl_test();
        etl.rule = EtlRule::Clean;
        etl.tables.append(EtlTable { schema: Some("app".into()), table: "items".into(), source: None });
        let schema = "app".to_string();
        let ec = EtlComposer::new(&etl, &dbc, &schema, &yt, true, &file, &mut rdr, &log).unwrap();
        assert_eq!(ec.columns_pklst, "id, code");
        assert!(ec.update, "listed and forced");
        assert_eq!(ec.pk_defaults.get("id").map(String::as_str), Some("nextval('items_id_seq'::regclass)"));

        let mut record = StringRecord::new();
        let mut keep = String::new();
        assert!(rdr.read_record(&mut record).unwrap());
        assert_eq!(ec.insert_statement_pk(&record), "INSERT INTO app.items (id, code, name, note) VALUES (1, 'A', 'O''Brien', '') \
            ON CONFLICT (id, code) DO UPDATE SET name='O''Brien', note=''");
        ec.values_inpk(&record, &mut keep);
        assert!(rdr.read_record(&mut record).unwrap());
        ec.values_inpk(&record, &mut keep);
        assert_eq!(keep, "(1, 'A'), (2, 'B')", "the keys a clean keeps");

        assert_eq!(ec.build_sequence(&"id".to_string(), &"nextval('items_id_seq'::regclass)".to_string()).unwrap(),
            "SELECT setval('app.items_id_seq', coalesce(max(id), 0)) + 1 FROM app.items");
        assert_eq!(ec.build_sequence(&"id".to_string(), &"nextval('other.s')".to_string()).unwrap(),
            "SELECT setval('other.s', coalesce(max(id), 0)) + 1 FROM app.items");
        assert_eq!(ec.build_sequence(&"code".to_string(), &"'A'::text".to_string()), None);
        fs::remove_file(&file);
    }
}
