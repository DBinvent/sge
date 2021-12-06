use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Display;
use std::{fmt, fs};
use std::cmp::{max, Ordering};
use std::fs::File;
use std::path::Path;
use csv::{Reader, StringRecord};
use postgres::{Client, Row, Transaction};
use postgres::types::Type;
use semver::Version;
use slog::Logger;
use crate::{ConfigMap, FileFilter, FileVersion, InfoSchemaType, LoadResult, LogSchemaType, Named, OrderedHashMap, StatementLog, YEtl, YSchema, YTable};
use serde::Serialize;

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
fn as_string(row: &Row, idx: usize, t: &YTable) -> Result<String, String> {
    let c = &row.columns()[idx];
    let nullable = match t.columns.get(&c.name().to_string()) {
        None => {return Err(format!("column {} not found during data load. try to run again", c.name()));}
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
            &Type::TIMESTAMPTZ => match row.get::<_, Option<time::OffsetDateTime>>(idx){
                None => "".to_string(),
                Some(x) => x.to_string()
            },
            t @ _ => match row.try_get::<_, Option<String>>(idx)
                .map_err(|e| format!("converting type [{}] to string: {}", t.name(), e))? {
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
            &Type::TIMESTAMPTZ => row.get::<_, time::OffsetDateTime>(idx).to_string(),
            t @ _ => row.try_get::<_, String>(idx)
                .map_err(|e| format!("converting type [{}] to string: {}", t.name(), e))?
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
            table_tr: cfg.get("schema_transaction") == "table",
            yaml_format: cfg.get_def("out_format", "yaml".to_lowercase()).to_lowercase().as_str() == "yaml",
            ver: cfg.get_opt("snapshot_version"),
            dry_run: cfg.get_bool("dry_run", false),
            dry_run_to: cfg.get_opt("dry_run_to")
        })
    }

    /// no schema
    #[inline]
    fn find_set<'a>(&self, slf: &'a mut LoadResult, table: &String) -> Result<(String, String, String, &'a YTable), String> {
        for s in slf.database.iter_mut() {
            if let Some(t) = s.tables.get_mut(table) {
                let dump_to = self.dump_to(format!("{}_{}.csv", s.schema_name, table))?;
                t.etl = Some(YEtl{ file: Some(dump_to.clone()), view: None });
                return Ok((dump_to, s.schema_name.clone(), table.clone(), t));
            }
        }
        Err(format!("No schema found for table: {}", table))
    }

    /// with schema
    #[inline]
    fn find_sets<'a>(&self, slf: &'a mut LoadResult, schema: &str, table: &String) -> Result<(String, String, String, &'a YTable), String> {
        for s in slf.database.iter_mut() {
            if s.schema_name.as_str() == schema {
                return match s.tables.get_mut(&table) {
                    None => Err(format!("Table or schema not found: {}.{}", schema, table)),
                    Some(t) => {
                        let dump_to = self.dump_to(format!("{}_{}.csv", s.schema_name, table))?;
                        t.etl = Some(YEtl{ file: Some(dump_to.clone()), view: None });
                        Ok((dump_to, s.schema_name.clone(), table.clone(), t))
                    }
                }
            }
        }
        Err(format!("No schema found for: {}.{}", schema, table))
    }
    // LoadResult

    /// same as  COPY ttest TO '/tmp/ttest_dn.csv' csv;
    pub(crate) fn extract(&self, slf: &mut LoadResult, db_client: &mut Client, log: &Logger) -> Result<(), String> {
        for t in &self.tables.list {
            if t.table.len() > 0 {
                let (file, schema, table, yt) = match &t.schema {
                    None => self.find_set(slf, &t.table)?,
                    Some(s) => self.find_sets(slf, s.as_str(), &t.table)?
                };
                let mut write_header = true;
                let msg = format!("ETL: CSV File '{}'", file);
                let mut w = csv::Writer::from_path(file.as_str())
                    .map_err(|e| format!("{}, create error: {}", msg, e))?;
                let mut c = 0;
                let source = t.source.clone().unwrap_or(format!("{}.{}", schema, table));
                let sql = format!("select * from {}", source);
                for r in db_client.query(sql.as_str(), &[])
                        .map_err(|e| format!("ETL: Load from {}, with error: {}", source, e))? {
                    if write_header {
                        for i in 0..r.len() {
                            let c = &r.columns()[i];
                            w.write_field(c.name()).map_err(|e| format!("{}: write header error: {}", msg, e))?;
                        }
                        let _ = w.write_record(None::<&[u8]>).map_err(|e| format!("{}: write EOL header error: {}", msg, e))?;
                        write_header = false;
                    }
                    for i in 0..r.len() {
                        let _ = w.write_field(as_string(&r, i, yt)?).map_err(|e| format!("{}: write record error: {}", msg, e))?;
                    }
                    let _ = w.write_record(None::<&[u8]>).map_err(|e| format!("{}: write EOL{} error: {}", msg, c, e))?;
                    c += 1;
                }
                if c == 0 {
                    info!(log, "ETL: No data for {}.{} by sql [{}] therefor the file {} is empty!", schema, table, sql, file);
                } else {
                    debug!(log, "ETL: Extracted [{}] records into {}", c, file);
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
                            let _ = fs::create_dir_all(&file[..i]).map_err(|e| format!("writing to dir {} : {}", &file, e))?;
                        }

                        let file = if Path::new(&file).is_dir() {
                            self.dump_to(format!("{}.yaml", self.db_name))?
                        } else { file.clone() };

                        let _ = fs::write(&file, loaded).map_err(|e| format!("writing to {} : {}", file, e))?;
                    },
                }
                Ok(())
            }
            Err(e) => Err(e)
        }
    }

    /// generate a NEXT file name ends with 'ftype', build dir.
    #[inline]
    fn dump_to(&self, ftype: String) -> Result<String, String> {
        let mut i = 0;
        let _ = fs::create_dir_all(&self.dir).map_err(|e| format!("Creating {} {}", self.dir, e));
        for path in fs::read_dir(&self.dir)
            .map_err(|e| format!("Reading {} {}", self.dir, e))? {

            let p = path.map_err(|e| format!("Reading {} {}", self.dir, e))?;
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
    fn find(&self, schema: &String, table: &YTable) -> bool {
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

    pub(crate) fn deploy_all(
        self,
        ysv: &HashMap<String, YSchema>,
        logs: &LogSchemaType,
        dbc: &InfoSchemaType,
        db: &mut Transaction,
        lt: &mut Transaction,
        clog_t: &mut Option<Transaction>,
        stl: &StatementLog,
        log: &Logger,
    ) -> Result<bool, String> {
        let mut datas = DataTableMap::new();
        let fp = FileFilter::newt(".csv");
        let mut do_persist = false;
        for (s, ys) in ysv {
            for yt in &ys.tables.list {
                if let Some(data) = &yt.etl {
                    match &data.file {
                        Some(data_file) => {
                            let file = format!("{}{}", self.dir, data_file);
                            let update = match self.rule {
                                EtlRule::Ignore => {
                                    if !Path::new(file.as_str()).exists() {
                                        warn!(log, "ETL: {} table ignore non existing file {}", yt.table_name, file);
                                        return Ok(false);
                                    }
                                    false
                                }
                                EtlRule::Skip => {
                                    if self.tables.len() == 0 {
                                        info!(log, "ETL: skipped on file {} (and all other)", file);
                                        return Ok(false);
                                    } else if self.find(s, yt) {
                                        info!(log, "ETL: skip file {} for table {}", file, yt.table_name);
                                        continue;
                                    }
                                    false
                                }
                                EtlRule::Clean | EtlRule::Force => true,
                                EtlRule::Apply => false
                            };
                            let readable = { File::open(&file).map_err(|e| format!("Can't read file: {}: {}", file, e))?; true };
                            if readable {
                                let data_file_ver = DataFileVersion::new(data_file, &fp, Some(yt));
                                let dstl = stl.instance('D', &data_file_ver.v.s, &self.dir, data_file);
                                {
                                    if let Some((_sl_file, checks)) = logs.get(&data_file_ver.file) {
                                        if checks == &dstl.checksum {
                                            info!(log, "ETL: skip unchanged file {} for table {} [{}]", file, yt.table_name, data_file_ver.file);
                                            continue;
                                        }
                                    };
                                }
                                datas.insert(data_file_ver, (&s, yt, update, file, dstl));
                            } else {
                                return Err(format!("ETL: data file {} for table {} is not readable", data_file, yt.table_name));
                            }

                        }
                        None => { // no CSV file
                            if let Some(view) = &data.view {
                                return Err(format!("ETL: generate data for table {} by view {} is not implemented yet", yt.table_name, view));
                            }
                        }
                    }
                }
            }
        }

        if datas.len() > 0 {
            debug!(log, "ETL: uploading [{}] file(s)", datas.len());
        }

        for (schema_name, yt, update, file, dstl) in datas.values() {
            let mut r = csv::Reader::from_path(file)
                .map_err(|e| format!("ETL: Reading {}: {}", file, e))?;
            match EtlComposer::new(&self, dbc, *schema_name, *yt, *update, file, &mut r, log)?
                .deploy(&mut r, db) {
                Ok(ok) => {
                    if ok {
                        let _ = dstl.persist(None, Some(lt), clog_t)?;
                        do_persist = !self.dry_run;
                        if self.table_tr && !self.dry_run {
                            if let Err(ec) = db.batch_execute("COMMIT; BEGIN") {
                                let _ = dstl.persist(Some(ec.to_string()), Some(lt), clog_t)?;
                                return Err(format!(
                                    "committing data change on {}.{} with {}", schema_name, yt.table_name, ec
                                ));
                            }
                        }
                    } else {
                        info!(log, "ETL: no data loaded from {} for table {}", file, yt.table_name);
                    }
                }
                Err(err) => {
                    let _ = dstl.persist(Some(err.clone()), Some(lt), clog_t)?;
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
    // TODO FIXME NOW test
    fn new(data_file: &String, fp: &FileFilter, yto: Option<&YTable>) -> Self {
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
               Some(yt) => max(yt.weight, yt.pseudo_weight())
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

type DataTableMap<'a> = BTreeMap<DataFileVersion, (&'a String, &'a YTable, bool, String, StatementLog)>;

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
    yaml_table: &YTable,
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
        yaml_table: &'a YTable,
        do_force_update: bool,
        src_file: &String, // to use for logging only
        csv_reader: &mut Reader<File>,
        log: &'a Logger,
    ) -> Result<Self, String>
    {
        let headers = csv_reader.headers().map_err(|e| format!("Reading {}: {}", src_file, e))?.clone();
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
    fn deploy(&self, csv_reader: &mut Reader<File>, db: &mut Transaction) -> Result<bool, String>
    {
        let mut cnt = 0;
        let mut clean_sql = String::new();
        let mut record = csv::StringRecord::new();
        let clean = match self.etl.rule {
            EtlRule::Clean => true,
            _ => false
        };
        info!(self.log, "ETL: Loading data from {} into {} using {}", self.file, self.table, self.etl.rule);
        while csv_reader.read_record(&mut record)
            .map_err(|e| format!("Reading {} {}", self.file, e))?
        {
            let sql = if self.hidx_pks_clmn.len() == 0 {
                if clean {
                    warn!(self.log, "ETL: --etl=clean not allowed for table {} contains no PrimaryKey", self.table);
                }
                self.insert_statement_nopk(&record)
            } else {
                if clean {
                    self.values_inpk(&record, &mut clean_sql);
                }
                self.insert_statement_pk(&record)
            };

            cnt += self.execute("insert/update", sql, db)?;
        }
        let cleaned = if clean_sql.len() > 0 {
            let (lb, rb) = if self.hidx_pks_clmn.len() > 1 { ('(',')') } else { (' ', ' ')};
            self.execute("clean update", format!("DELETE FROM {} WHERE {}{}{} NOT IN ({})", self.table, lb, self.columns_pklst, rb, clean_sql), db)?
        } else { 0 };
        if cleaned > 0 {
            info!(self.log, "ETL: Cleaned {} records, from {} by {}", cleaned, self.file, self.table);
        }
        if cnt > 0 {
            for (pk, def) in &self.pk_defaults {
                if let Some(sql) = self.build_sequence(pk, def) {
                    let _ = self.execute("sequence",sql, db)?;
                }
            }
            info!(self.log, "ETL: Loaded {} records, from {} into {}", cnt, self.file, self.table);
        }
        Ok(cleaned + cnt > 0)
    }

    fn execute(&self, method: &str, sql: String, db: &mut Transaction) -> Result<u64, String> {
        if self.etl.dry_run {
            let _ = StatementLog::dry_store(self.etl.dry_run_to.clone(), sql, &self.log)?;
            Ok(0)
        } else {
            #[cfg(debug_assertions)] debug!(self.log, "ETL_{}: {}", method, sql);
            db.execute(sql.as_str(), &[])
                .map_err(|e| format!("ETL {} execute for {}: raise {}", method, self.table, e))
        }
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
    use sloggers::Build;
    use sloggers::terminal::*;
    use sloggers::types::*;
    use uuid::Uuid;
    use crate::YColumn;
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
            ver: None,
            dry_run: false,
            dry_run_to: None
        }
    }

    fn yt_test() -> YTable {
        let mut columns = OrderedHashMap::new();
        columns.append(YColumn::newt("r", "serial", true, false));
        columns.append(YColumn::newt("active", "", false, true));
        columns.append(YColumn::newt("starts_at", "date", false, true));
        columns.append(YColumn::newt("ends_at", "date", false, true));
        columns.append(YColumn::newt("cap", "varchar", false, true));
        YTable {
            table_name: "rdict".to_string(),
            description: "".to_string(),
            transaction: "".to_string(),
            sql: "".to_string(),
            constraint: "".to_string(),
            columns,
            triggers: OrderedHashMap::new(),
            etl: None,
            weight: 0
        }
    }

    #[inline]
    fn uuid() -> String {
        let x = Uuid::new_v4();
        x.to_hyphenated().to_string()[..8].into()
    }

    #[test]
    fn w_test() {
        let log = logger(true);
        let file = format!("/tmp/{}.csv", uuid());
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
}
