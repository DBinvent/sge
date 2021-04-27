extern crate yaml_rust;

use std::collections::{BTreeMap, HashMap};
use std::fs;

use postgres::Transaction;
use semver::{SemVerError, Version};
use yaml_rust::YamlLoader;

use crate::dbm_dbc::{FKTable, InfoSchemaType, STable};

use self::yaml_rust::Yaml;
use serde::{Deserialize, Serialize};
use std::fmt::{Formatter, Write};
use std::result::Result;

use crate::dbm_log::StatementLog;
use clap::ArgMatches;
use slog::Logger;
use std::cmp::Ordering;
use std::fmt;
use std::fmt::Display;

/// version is a [parse_type]
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FilesMap {
    /// version: schemaName : schema (with name)
    pub schemas: SchemaTableMap,
    /// version: file
    pub pre: VersionTableMap,
    /// version: file
    pub versions: VersionTableMap,
    /// version: file
    pub re_runnable: VersionTableMap,
    /// After scritps
    pub after: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct FileVersion {
    #[serde(skip_serializing)]
    v: Version,
    pub s: String,
}

pub(crate) type SchemaTableMap = BTreeMap<FileVersion, (String, HashMap<String, YSchema>)>;
pub(crate) type VersionTableMap = BTreeMap<FileVersion, String>;

impl FileVersion {
    #[inline]
    fn new(vv: Version, key: String) -> Self {
        FileVersion { v: vv, s: key }
    }
    #[inline]
    #[allow(dead_code)]
    fn from(key: &str) -> Result<Self, SemVerError> {
        Ok(FileVersion {
            v: Version::parse(key)?,
            s: key.into(),
        })
    }
}

impl PartialEq for FileVersion {
    fn eq(&self, other: &Self) -> bool {
        self.v == other.v
    }
}

impl Eq for FileVersion {}

impl PartialOrd for FileVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.v.cmp(&other.v))
    }
}

impl Ord for FileVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.v.cmp(&other.v)
    }
}
impl Display for FileVersion {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.s)
    }
}

pub(crate) fn load_all(cfg: &ConfigMap, log: &Logger) -> Result<FilesMap, String> {
    let mut schema = FilesMap::new();
    load_dir(
        cfg.get("source_dir", "./").as_str(),
        &mut schema,
        &FileFilters::new(cfg)?,
        cfg.get_bool("source_recur", true), log
    )?;
    Ok(schema)
}

impl FilesMap {
    fn new() -> Self {
        FilesMap {
            pre: Default::default(),
            schemas: Default::default(),
            versions: Default::default(),
            re_runnable: Default::default(),
            after: Default::default(),
        }
    }

    /// looking for FKtable's PK
    #[inline]
    fn pks(&self, schema: &String, table: &String) -> String {
        let mut pk = String::new();
        for (_, (_, sks)) in &self.schemas {
            if let Some(fkst) = sks.get(schema) {
                if let Some(tt) = fkst.tables.get(table) {
                    for cc in &tt.columns.list {
                        if cc.is_pk() {
                            let _ = write!(pk, "{}{}", if pk.len() > 0 { ", " } else { "" }, cc.get_name());
                        }
                    }
                }
            }
        }
        pk
    }
}

#[derive(Debug, Clone)]
struct FileFilters {
    pub pre: FileFilter,
    pub schema_deploy: FileFilter,
    pub version_deploy: FileFilter,
    pub rr_deploy: FileFilter,
}

impl FileFilters {
    #[inline]
    fn new(cfg: &ConfigMap) -> Result<Self, String> {
        Ok(FileFilters {
            pre: FileFilter::new("pre_deploy", cfg, ".sql")?,
            schema_deploy: FileFilter::new("schema_deploy", cfg, ".yaml")?,
            version_deploy: FileFilter::new("version_deploy", cfg, ".sql")?,
            rr_deploy: FileFilter::new("rr_deploy", cfg, ".sql")?,
        })
    }
}

#[inline(never)]
fn load_dir(
    dir: &str,
    schema: &mut FilesMap,
    ff: &FileFilters,
    recursive: bool,
    log: &Logger
) -> Result<(), String> {
    match fs::read_dir(dir) {
        Ok(paths) => {
            for path in paths {
                match path {
                    Ok(p) => {
                        let fm = p.metadata().unwrap();
                        let file_name = p.path().to_string_lossy().to_string();
                        if fm.is_dir() && recursive {
                            load_dir(file_name.as_str(), schema, ff, recursive, log)?;
                        } else if fm.is_file() && file_name.len() > 5 {
                            let starting_pos = match file_name.rfind("/") {
                                None => 0,
                                Some(i) => i + 1,
                            };
                            match file_name.as_bytes()[starting_pos] {
                                b'S' => {
                                    if let Some((vv, key)) =
                                        ff.schema_deploy.parse_type(&file_name, starting_pos)?
                                    {
                                        match load(file_name.as_str()) {
                                            Ok(y) => match y["database"].as_vec() {
                                                None => {}
                                                Some(schemas) => {
                                                    let mut schema_schemas = HashMap::new();

                                                    for s in schemas {
                                                        let ysn = YSchema::schema_name(s);
                                                        if !schema_schemas.contains_key(&ysn) {
                                                            schema_schemas.insert(
                                                                ysn.clone(),
                                                                YSchema::new(&s, &file_name),
                                                            );
                                                        }
                                                        if let Some(ss) =
                                                            schema_schemas.get_mut(&ysn)
                                                        {
                                                            let _ = ss.append(s, log)?;
                                                        }
                                                    }
                                                    schema.schemas.insert(
                                                        FileVersion::new(vv, key),
                                                        (file_name, schema_schemas),
                                                    );
                                                }
                                            },
                                            Err(e) => {
                                                return Err(format!("{}", e));
                                            }
                                        }
                                    }
                                }
                                b'P' => {
                                    if let Some((vv, key)) =
                                        ff.pre.parse_type(&file_name, starting_pos)?
                                    {
                                        schema.pre.insert(FileVersion::new(vv, key), file_name);
                                    }
                                }
                                b'V' => {
                                    if let Some((vv, key)) =
                                        ff.version_deploy.parse_type(&file_name, starting_pos)?
                                    {
                                        schema
                                            .versions
                                            .insert(FileVersion::new(vv, key), file_name);
                                    }
                                }
                                b'R' => {
                                    if let Some((vv, key)) =
                                        ff.rr_deploy.parse_type(&file_name, starting_pos)?
                                    {
                                        schema
                                            .re_runnable
                                            .insert(FileVersion::new(vv, key), file_name);
                                    }
                                }
                                b'a' => {
                                    if file_name.ends_with(".sql") {
                                        schema.after.insert(
                                            file_name[starting_pos..file_name.len() - 4].into(),
                                            file_name,
                                        );
                                    }
                                }
                                _ => {
                                    // unknown file code type
                                }
                            }
                        }
                    }
                    Err(e) => {
                        return Err(format!("read error: {}", e));
                    }
                }
            }
            Ok(())
        }
        Err(e) => Err(format!("dir read error [{}]: {}", dir, e)),
    }
}

#[derive(Debug, Clone)]
struct FileFilter {
    /// None for any
    filter: Option<Version>,
    file_type: String,
}

impl FileFilter {
    #[inline]
    fn new(field: &str, cfg: &ConfigMap, file_type: &str) -> Result<Self, String> {
        let f = cfg.get(field, "*");
        let filter = if f.as_bytes()[0] == b'*' {
            None
        } else {
            Some(semver_parse(f.as_str())?)
        };
        Ok(FileFilter {
            filter,
            file_type: file_type.into(),
        })
    }

    #[inline]
    fn parse_type(
        &self,
        filename: &String,
        from_pos: usize,
    ) -> Result<Option<(Version, String)>, String> {
        if filename.ends_with(&self.file_type) {
            match filename.find("__") {
                None => Err(format!("unrecognizable version of file: {}", filename)),
                Some(i) => {
                    let ver = filename[from_pos + 1..i].replace("_", ".");
                    match &self.filter {
                        None => Ok(Some((semver_parse(ver.as_str())?, ver))),
                        Some(fv) => {
                            let vv = semver_parse(ver.as_str())?;
                            if fv >= &vv {
                                Ok(Some((vv, ver)))
                            } else {
                                Ok(None)
                            }
                        }
                    }
                }
            }
        } else {
            Ok(None)
        }
    }
}

#[inline]
pub(crate) fn semver_parse(input: &str) -> Result<Version, String> {
    let fd = match input.find(".") {
        None => 0,
        Some(i) => i,
    };
    let ld = match input.rfind(".") {
        None => 0,
        Some(i) => i,
    };
    let input = if fd < ld {
        input.to_string()
    } else if fd == 0 {
        format!("{}.0.0", input)
    } else {
        format!("{}.0", input)
    };
    match semver::Version::parse(input.as_str()) {
        Ok(v) => Ok(v),
        Err(e) => Err(format!("version parsing: {} error: {}", input, e)),
    }
}

#[inline]
fn load(filename: &str) -> Result<Yaml, String> {
    match fs::read_to_string(filename) {
        Ok(data) => match YamlLoader::load_from_str(data.as_str()) {
            Ok(mut y) => Ok(y.remove(0)),
            Err(e) => Err(format!("parsing error: {} of file: {}", e, filename)),
        },
        Err(e) => Err(format!("load error [{}]: {}", filename, e)),
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct YSchema {
    #[serde(rename = "schemaName")]
    pub schema_name: String,
    pub owner: String,
    /// tableName: table(with name)
    pub tables: OrderedHashMap<YTable>,
    /// the table definition loaded from file
    pub file: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct YTable {
    #[serde(rename = "tableName")]
    pub table_name: String,
    /// comments
    pub description: String,
    /// transaction: -- single (default) OR table OR column OR retry (wrap to psql)
    pub transaction: String,
    /// suffix on table create
    pub sql: String,
    pub constraint: String,
    pub columns: OrderedHashMap<YColumn>,
    pub triggers: OrderedHashMap<YTrig>,
}

pub trait Named {
    fn get_name(&self) -> String;
}

impl Named for YTable {
    fn get_name(&self) -> String {
        self.table_name.clone()
    }
}

impl Named for YColumn {
    fn get_name(&self) -> String {
        self.name.clone()
    }
}

impl Named for YTrig {
    fn get_name(&self) -> String {
        self.name.clone()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct YColumn {
    pub name: String, // id
    #[serde(rename = "type")]
    pub column_type: String, // serial
    #[serde(rename = "defaultValue")]// pub defaultValue: String, //
    pub default_value: String, //
    pub constraint: Option<YConstr>,//# changes on PK & NULL wont apply
    pub description: String, //
    pub sql: String, // -- some SQL suffix on new column create
}

#[derive(Debug, Clone, Serialize)]
pub struct YConstr {
    #[serde(rename = "primaryKey")]// pub primaryKey: bool, //true
    pub primary_key: bool, //true
    pub nullable: bool, //false
    #[serde(rename = "foreignKey")]// pub foreignKey: Option<YForeignKey>
    pub foreign_key: Option<YForeignKey>
}

#[derive(Debug, Clone, Serialize)]
pub struct YForeignKey {
    pub references: String, //fk_table
    pub sql: String, //-- some SQL suffix on new FK create- on delete no action on update no action
}

#[derive(Debug, Clone, Serialize)]
pub struct YTrig {
    pub name: String, //uniq_name_of_trigger
    pub event: String, // before update
    pub when: String, //  for each row
    pub proc: String, //  -- p()
}

impl YColumn {
    fn new(input: &Yaml) -> Self {
        let constraint = &input["constraint"];
        let foreign_key = &constraint["foreignKey"];
        let references = as_str_esc(foreign_key, "references");
        let (foreign_key, fk_set) = if references.len() == 0 { (None, false) } else {
            (Some( YForeignKey{ references, sql: as_str_esc(foreign_key, "sql")}), true)
        };
        let primary_key = as_bool(constraint, "primaryKey", false);
        let nullable = as_bool(constraint, "nullable", true);
        let constraint = if primary_key || !nullable || fk_set {
            Some( YConstr{ primary_key, nullable, foreign_key})
        } else { None };
        YColumn {
            name: safe_sql( as_str_esc(input, "name")),
            column_type:  as_str_esc(input, "type"),
            default_value:  as_str_esc(input, "defaultValue"),
            description:  as_str_esc(input, "description"),
            sql:  as_str_esc(input, "sql"),
            constraint,
        }
    }

    pub(crate) fn is_pk(&self) -> bool {
        match &self.constraint {
            None => false,
            Some(c) => c.primary_key,
        }
    }

    /// column defenition to SQL string
    #[inline]
    fn column_def(&self, schema: &String, table_name: &String, file: &str) -> Result<String, String> {
        if self.column_type.len() == 0 {
            Err(format!(
                "Error column definition on table: {}.{} column {} file {}",
                schema, table_name, self.name, file
            ))
        } else {
            let mut cs = String::new();
            if let Some(constraint) = &self.constraint {
                if constraint.primary_key {
                    let _ = write!(cs, " primary key ");
                }
                if !constraint.nullable{
                    let _ = write!(cs, " not null ");
                }
            }
            Ok(format!(
                "{} {}{}{}{}{}",
                self.name,
                self.column_type,
                cs,
                if self.default_value.len() > 0 { " default " } else { "" },
                self.default_value,
                self.sql
            ))
        }
    }

}

impl YTrig {
    fn new(input: &Yaml) -> Self {
        YTrig {
            name: safe_sql( as_str_esc(input, "name")),
            event: as_str_esc(input, "event"),
            when: as_str_esc(input, "when"),
            proc: as_str_esc(input, "proc")
        }
    }

        /// column defenition to SQL string
    #[inline]
    // pub(crate)
    fn trig_def(&self, schema: &String, table_name: &String) -> Option<String> {
        // let proc = as_str_esc(input, "proc");
        if self.proc.len() > 0 {
            Some(format!(
                "CREATE TRIGGER {} {} ON {}.{} {} EXECUTE PROCEDURE {};",
                self.name,
                self.event,
                schema,
                table_name,
                self.when,
                self.proc
            ))
        } else {
            None
        }
    }

}

#[inline]
fn as_str_esc(input: &Yaml, field: &str) -> String {
    let val = as_str(input, field, "");
    match val.find(" --") {
        None => {
            if val.len() > 0 {
                format!("{}", val)
            } else {
                "".into()
            }
        }
        Some(i) => val[0..i].into(),
    }
}

#[inline]
fn as_str(input: &Yaml, field: &str, def: &str) -> String {
    if input.is_null() {
        def.into()
    } else {
        match input[field].as_str() {
            None => def.into(),
            Some(s) => s.into(),
        }
    }
}

#[inline]
fn as_bool(input: &Yaml, field: &str, default: bool) -> bool {
    if input.is_null() {
        default
    } else {
        match input[field].as_str() {
            None => default,
            Some(s) => str2bool(s, default),
        }
    }
}

#[inline]
fn str2bool(input: &str, default: bool) -> bool {
    if input.len() == 0 {
        default
    } else {
        input == "+"
            || input == "yes"
            || input == "YES"
            || input == "true"
            || input == "TRUE"
            || input == "ok"
            || input == "OK"
            || input == "on"
            || input == "ON"
            || input == "y"
            || input == "Y"
            || input == "T"
            || input == "1"
    }
}

#[inline]
fn safe_sql(input: String) -> String {
    match input
        .chars()
        .position(|c| c == ' ' || c == '.' || c == ';' || c == '\n' || c == '\t')
    {
        None => input,
        Some(i) => input[0..i].into(),
    }
}

/// String key for BTreeMap, sorted by adding order
#[derive(Debug, Clone, Serialize)]
pub struct OrderedHashMap<T: Named> {
    #[serde(skip)]
    map: BTreeMap<String, usize>,
    // TODO implement #[serde(flatten)] for this as remove list field
    pub(crate) list: Vec<T>,
}

impl<T: Named> OrderedHashMap<T> {
    #[inline]
    pub(crate) fn new() -> Self {
        OrderedHashMap {
            map: Default::default(),
            list: vec![],
        }
    }
    #[inline]
    pub(crate) fn append(&mut self, value: T) -> Result<(), String> {
        let key_name = value.get_name();
        if key_name.len() == 0 {
            Err("Empty".into())
        } else if self.map.contains_key(&key_name) {
            Err(format!("Duplicate {}", key_name))
        } else {
            self.map.insert(key_name, self.list.len());
            self.list.push(value);
            Ok(())
        }
    }
    #[inline]
    pub(crate) fn get(&self, key: &String) -> Option<&T> {
        match self.map.get(key) {
            None => None,
            Some(id) => Some(&self.list[*id]),
        }
    }
    #[inline]
    pub fn len(&self) -> usize {
        self.list.len()
    }
}

impl YTable {
    pub(crate) fn new(input: &Yaml, table_name: &str, file: Option<&String>, log: Option<&Logger>) -> Result<Self, String> {
        let mut columns = OrderedHashMap::new();
        if let Some(cls) = input["columns"].as_vec() {
            let mut i = 1;
            for cl in cls {
                let c = &cl["column"];
                if !c.is_null() {
                    if let Some(_name) = c["name"].as_str() {
                        let yc = YColumn::new(c);
                        match file {
                            None => {
                                // println!("{}:\n{:?}\n{:?}", _name, yc, c);
                            }
                            Some(_f) => {
                                if let Some(log) = log {
                                    debug!(log, "+\n{:?}\n{:?}", yc, c);
                                }
                            }
                        }
                        if let Err(e) = columns.append(yc) {
                            return Err(format!(
                                "{} (column name) {}/{} on table: {}{}{}",
                                e, i, cls.len(), table_name,
                                match file { None => "", Some(_) => ", found in file: " },
                                match file { None => "", Some(f) => f.as_str(), },
                            ));
                        };
                    }
                    i += 1;
                }
            }
        }
        let mut triggers = OrderedHashMap::new();
        if let Some(trs) = input["triggers"].as_vec() {
            for tr in trs {
                let t = &tr["trigger"];
                if !t.is_null() {
                    if let Some(name) = t["name"].as_str() {
                        if name.len() > 0 {
                            if let Err(_) = triggers.append(YTrig::new(t)) {
                                return Err(format!(
                                    "Duplicate trigger name: {} on table: {}{}{}",
                                    name, table_name,
                                    match file {
                                        None => "",
                                        Some(_) => ", found in file: "
                                    },
                                    match file {
                                        None => "",
                                        Some(f) => f.as_str(),
                                    },
                                ));
                            };
                        }
                    }
                }
            }
        }
        Ok(YTable {
            table_name: table_name.into(),
            description: as_str(input, "description", ""),
            transaction: as_str(input, "transaction", ""),
            sql: as_str_esc(input, "sql"),
            constraint: as_str_esc(input, "constraint"),
            columns,
            triggers,
        })
    }

    /// build a create or alter sql
    #[allow(unused_mut)]
    pub(crate) fn deploy(
        &self,
        dbc: &mut InfoSchemaType,
        db: &mut Transaction,
        schema: &String,
        cfg: &ConfigMap,
        file: &str,
        log: &Logger,
    ) -> Result<bool, String> {
        let mut sql = String::new();
        let mut comments = String::new();
        let mut exec = false;
        let (create_schema, create_table) = match dbc.get_mut(schema) {
            None => (true, true),
            Some(mut ss) => match ss.get_mut(&self.table_name) {
                None => (false, true),
                Some(mut ts) => {
                    for  dc in &self.columns.list {
                        if !ts.columns.contains_key( &dc.name) {
                            let def = dc.column_def( schema, &self.table_name, file)?;
                            let _ = write!(
                                sql,
                                "ALTER TABLE {}.{} ADD COLUMN {};",
                                schema, self.table_name, def
                            );
                            let _ = ts.columns.insert(dc.get_name(), def);
                            exec = true;
                        }
                    }
                    for  dt in &self.triggers.list {
                        if !ts.triggers.contains_key(&dt.name) {
                            if let Some(def) = dt.trig_def(schema, &self.table_name) {
                                let _ = writeln!(sql, "{}", def);
                                let _ = ts.triggers.insert(dt.get_name(), def);
                                exec = true;
                            }
                        }
                    }
                    (false, false)
                }
            },
        };
        if create_schema {
            dbc.insert(schema.clone(), HashMap::new());
        }
        if create_table {
            let owner = "";
            let mut columns = String::new();
            let mut st = STable {
                columns: HashMap::new(),
                fks: Default::default(),
                triggers: HashMap::new(),
            };
            for  dc in &self.columns.list {
                let cd = dc.column_def(schema, &self.table_name, file)?;
                let _ = write!(columns, "{}, ", cd);
                let _ = st.columns.insert(dc.get_name(), cd);
                self.comments(&mut comments, schema, &dc.name, &dc.description);
            }
            if columns.len() > 0 {
                if create_schema && schema.as_str() != "public" {
                    let _ = write!(sql, "CREATE SCHEMA IF NOT EXISTS {} ", schema);
                    if owner.len() > 0 {
                        let _ = write!(sql, "AUTHORIZATION {}", owner);
                    }
                    let _ = write!(sql, ";\n");
                }
                columns.remove(columns.rfind(",").unwrap());
                let _ = writeln!(
                    sql,
                    "CREATE TABLE {}.{} ({}{}{}){}; \n",
                    schema,
                    self.table_name,
                    columns,
                    if self.constraint.len() > 0 { ", " } else { "" },
                    self.constraint,
                    self.sql
                );
                if owner.len() > 0 {
                    let _ = writeln!(
                        sql,
                        "ALTER TABLE {}.{} OWNER TO {}; \n",
                        schema, self.table_name, owner
                    );
                }
            }
            for dt in &self.triggers.list {

                if let Some(td) = dt.trig_def(schema, &self.table_name) {
                    let _ = writeln!(sql, "{}\n", td);
                    st.triggers.insert(dt.get_name(), td);
                }
            }
            dbc.get_mut(schema)
                .unwrap()
                .insert(self.table_name.clone(), st);
            exec = true;
        }
        if exec {
            if self.description.len() > 0 {
                let _ = writeln!(
                    comments,
                    "COMMENT ON TABLE {}.{} IS '{}'; \n",
                    schema, self.table_name, self.description
                );
            }
        }
        if cfg.get_bool("dry_run", false) {
            match StatementLog::dry_store(cfg.get_opt("dry_run_to"), sql, log) {
                Ok(_) => {
                    let _ = StatementLog::dry_store(cfg.get_opt("dry_run_to"), comments, log);
                    Ok(false)
                }
                Err(e) => Err(format!("dry_run: [{}] {}", cfg.get("dry_run_to", ""), e)),
            }
        } else {
            if exec {
                match db.batch_execute(sql.as_str()) {
                    Ok(()) => {
                        let _ = db.batch_execute(comments.as_str());
                    }
                    Err(e) => {
                        return Err(format!("[{}]: {}", sql, e));
                    }
                }
            }
            Ok(exec)
        }
    }

    //YTable

    #[inline]
    fn _is_pk(&self, input: &Yaml) -> bool {
        let mut yes = false;
        let constraint = &input["constraint"];
        if !constraint.is_null() {
            if let Some(pk) = constraint["primaryKey"].as_bool() {
                yes = pk;
            }
        }
        yes
    }

    /// column defenition to SQL string
    #[inline]
    fn comments(&self, sql: &mut String, schema: &String, column_name: &String, t: &String) {
        // if let Some(t) = input["description"].as_str() {
            if t.len() > 0 {
                let _ = writeln!(
                    sql,
                    "COMMENT ON COLUMN {}.{}.{} IS '{}';",
                    schema, self.table_name, column_name, t
                );
            }

    }

    /// build a create or alter sql
    #[allow(unused, unused_mut)]
    pub(crate) fn deploy_fk(
        &self,
        yschemas: &FilesMap,
        dbc: &mut InfoSchemaType,
        db: &mut Transaction,
        schema: &String,
        cfg: &ConfigMap,
        _file: &str,
        log: &Logger,
    ) -> Result<bool, String> {
        let mut sql = String::new();
        let mut fk_list = Vec::new();
        if let Some(ss) = dbc.get(schema) {
            if let Some(ts) = ss.get(&self.table_name) {
                for dc in &self.columns.list {
                    if let Some(constraint) = &dc.constraint {
                        if let Some(fk) = &constraint.foreign_key {
                            let fk_table = &fk.references;
                            let (fk_schema, fk_table) = match fk_table.find(".") {
                                None => (schema.clone(), fk_table.to_string()),
                                Some(i) => (
                                    fk_table[0..i].to_string(),
                                    fk_table[i + 1..].to_string(),
                                ),
                            };
                            // check for FK already in DB
                            if let Some(a) = dbc.get(schema) {
                                if let Some(b) = a.get(&self.table_name) {
                                    if let Some(_) = b.fks.get(&dc.name) {
                                        continue; // FK found as already created in DB, check the next column
                                    }
                                }
                            }
                            let fk_columns = yschemas.pks(&fk_schema, &fk_table);
                            if fk_columns.len() > 0 {
                                fk_list.push(FKTable {
                                    column: fk_columns,
                                    name: dc.get_name(),
                                    schema: fk_schema,
                                    table: fk_table,
                                    sql: fk.sql.clone(),
                                });
                            }
                        }
                    }
                }
            }
        };
        let exec = fk_list.len() > 0;
        for ff in fk_list {
            if let Some(mut ss) = dbc.get_mut(schema) {
                if let Some(mut ts) = ss.get_mut(&self.table_name) {
                    ts.fks.insert(ff.name.clone(), ff.clone());
                }
            }
            let _ = write!(sql,
                "ALTER TABLE {}.{} ADD CONSTRAINT fk_{}_{}_{}_{} FOREIGN KEY ({}) REFERENCES {} ({}) {};",
                schema, self.table_name, schema, self.table_name, ff.schema, ff.table,
                ff.name, &ff.table, ff.column, ff.sql
            );
        }

        if cfg.get_bool("dry_run", false) && exec{
            match StatementLog::dry_store(cfg.get_opt("dry_run_to"), sql, log) {
                Err(e) => Err(format!("dry_run: [{}] {}", cfg.get("dry_run_to", ""), e)),
                Ok(_) => Ok(false),
            }
        } else {
            if exec {
                if let Err(e) = db.batch_execute(sql.as_str()) {
                    return Err(format!("DB FK execute [{}]: {}", sql, e));
                }
            }
            Ok(exec)
        }
    }
}
//YTable

impl YSchema {
    #[inline]
    fn schema_name(input: &Yaml) -> String {
        match input["schemaName"].as_str() {
            None => "public".into(),
            Some(s) => s.into(),
        }
    }

    // todo read Schema in JSON format
    #[inline]
    fn new(input: &Yaml, file: &String) -> Self {
        YSchema {
            schema_name: YSchema::schema_name(input),
            owner: as_str(input, "schemaName", ""),
            tables: OrderedHashMap::new(),
            file: file.clone(),
        }
    }

    #[inline]
    fn append(&mut self, input: &Yaml, log: &Logger) -> Result<(), String> {
        if let Some(tbls) = input["tables"].as_vec() {
            for t in tbls {
                match t["tableName"].as_str() {
                    None => {
                        return Err(format!("no table name set in file: {}", self.file));
                    }
                    Some(tn) => {
                        if self.tables.map.contains_key(tn) {
                            return Err(format!(
                                "duplicate table definition: {} found in file: {}",
                                tn, self.file
                            ));
                        } else {
                            let _ = self
                                .tables
                                .append( YTable::new(t, tn, Some(&self.file), Some(log) )?);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum ConfigMap<'a> {
    Cli(ArgMatches<'a>),
    Map((HashMap<String, String>, Yaml)),
}

#[derive(Debug, Deserialize)]
pub struct ConfigDefMap {
    params: HashMap<String, CfgMod>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CfgMod {
    /// param value
    pub val: String,
    /// param description
    pub dsr: String,
}

impl<'a> ConfigMap<'a> {
    #[inline]
    pub fn new(params: HashMap<String, String>) -> Self {
        ConfigMap::Map((params, crate::CONFIG_YAML["args"].clone()))
    }

    #[inline]
    pub fn get(&self, param: &str, default: &str) -> String {
        match &self {
            ConfigMap::Cli(cli) => match cli.value_of(param) {
                None => default.into(),
                Some(v) => v.into(),
            },
            ConfigMap::Map((map, cm)) => match map.get(param) {
                None => match cm[param].as_str() {
                    None => default.into(),
                    Some(x) => x.into(),
                },
                Some(x) => x.clone(),
            },
        }
    }

    #[inline]
    pub fn get_opt(&self, param: &str) -> Option<String> {
        match &self {
            ConfigMap::Cli(cli) => {
                if cli.occurrences_of(param) > 0 {
                    if let Some(v) = cli.value_of(param) {
                        return Some(v.into());
                    }
                }
            }
            ConfigMap::Map((map, _cm)) => {
                if let Some(x) = map.get(param) {
                    return Some(x.clone());
                }
            }
        }
        None
    }

    #[inline]
    pub(crate) fn write(&self, content: &mut String, prefix: &str, param: &str, amp: &str) {
        if let Some(v) = self.get_opt(format!("{}{}", prefix, param).as_str()) {
            let _ = write!(content, " {} {} = '{}'", amp, param, v);
        }
    }

    #[inline]
    pub(crate) fn get_pow(&self, content: &mut String, prefix: &str, param: &str, amp: &str) {
        if let Some(v) = self.get_opt(format!("{}{}", prefix, param).as_str()) {
            let _ = write!(content, "{}{}={}",
                if content.len() > 0 { amp } else { "" },
                param,
                v
            );
        }
    }

    #[inline]
    pub(crate) fn get_pow_scr(&self, content: &mut String, prefix: &str, param: &str, amp: &str) {
        if let Some(_) = self.get_opt(format!("{}{}", prefix, param).as_str()) {
            let _ = write!(content, "{}{}=***", if content.len() > 0 { amp } else { "" }, param);
        }
    }

    #[inline]
    pub fn get_bool(&self, param: &str, default: bool) -> bool {
        str2bool(
            self.get(param, format!("{}", default).as_str()).as_str(),
            default,
        )
    }

    /// find the target database name for log storage, use a database from connection if not set use '' as current db
    #[inline]
    pub(crate) fn target(&self) -> String {
        self.get("db_target", "")
    }

    #[inline]
    pub(crate) fn schema(&self) -> String {
        safe_sql(self.get("schema", "public"))
    }

    #[inline]
    pub(crate) fn history_table_name(&self) -> String {
        safe_sql(self.get("table", crate::STATEMENT_LOG_TBL.table_name.as_str()))
    }

    #[inline]
    pub(crate) fn is_pgsrv(&self) -> bool {
        if let Some(v) = self.get_opt("db_sslmode") {
            v.as_str() != "disable"
        } else { false }
    }

    #[inline]
    pub(crate) fn is_ssl(&self) -> bool {
        let mut result = false;
        if let Some(_) = self.get_opt("db_sslcert") {
            result = true;
        }
        if let Some(mode) = self.get_opt("db_sslmode") {
            result = result && mode.as_str() == "require"
        }
        result
    }
}

impl Display for CfgMod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.val)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defb_test() {
        defb_test_t(true);
        defb_test_t(false);
        assert!(str2bool("", true));
        defb_test_f(true);
        defb_test_f(false);
        assert!(!str2bool("", false));
    }

    fn defb_test_t(def: bool) {
        for v in vec!["true", "yes", "+", "Y", "OK", "ok"] {
            assert!(str2bool(v, def));
        }
    }

    fn defb_test_f(def: bool) {
        for v in vec!["false", "no"] {
            //
            assert!(!str2bool(v, def));
        }
    }

    #[test]
    fn iter_test() {
        let list = vec![
            FileVersion::from("0.0.3").unwrap(),
            FileVersion::from("0.0.1").unwrap(),
            FileVersion::from("0.0.2").unwrap(),
        ];
        let mut ff = BTreeMap::new();
        for l in list {
            ff.insert(l.clone(), l.s.clone());
        }
        let mut i = 1;
        for (f, _s) in ff {
            // println!("{}({})", f, s);
            assert_eq!(f.v.patch, i);
            i += 1;
        }
    }

    #[test]
    fn safe_test() {
        assert_eq!("a".to_string(), safe_sql("a;".to_string()));
        assert_eq!("a".to_string(), safe_sql("a".to_string()));
        assert_eq!("a".to_string(), safe_sql("a ".to_string()));
        assert_eq!("".to_string(), safe_sql("".to_string()));
        assert_eq!("a".to_string(), safe_sql("a. ".to_string()));
        assert_eq!("a".to_string(), safe_sql("a\n ".to_string()));
        assert_eq!("a".to_string(), safe_sql("a\t ".to_string()));

    }

    /*

    #[test]
    fn viter_test() {
        let mut map = OrderedHashMap::new();
        let _ = map.append("1".to_string());
        let _ = map.append("2".to_string());
        let _ = map.append("3".to_string());
        let mut i = 1;
        for (_, key) in &map.list {
            println!("{}", key);
            assert_eq!(key, &format!("{}", i));
            i += 1;
        }
    }
    */
}


/*

// todo use execution retry, wrap alter:

DO
$do$
DECLARE
   lock_timeout CONSTANT text := '50ms';
   max_attempts CONSTANT INT := 1000;
   ddl_completed BOOLEAN := FALSE;
BEGIN

   PERFORM set_config('lock_timeout', lock_timeout, FALSE);

   FOR i IN 1..max_attempts LOOP
      BEGIN
         EXECUTE 'ALTER TABLE test add column whatever2 INT4';
         ddl_completed := TRUE;
         EXIT;
      EXCEPTION
         WHEN lock_not_available THEN
           NULL;
      END;
   END LOOP;

   IF ddl_completed THEN
      RAISE INFO 'DDL has been successfully completed';
   ELSE
      RAISE EXCEPTION 'Failed to perform DDL';
   END IF;
END
$do$;
*/