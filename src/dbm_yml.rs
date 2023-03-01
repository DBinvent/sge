extern crate yaml_rust;

use std::collections::{BTreeMap, HashMap, BTreeSet, HashSet};
use std::fs;

use postgres::Transaction;
use semver::{SemVerError, Version};
use yaml_rust::YamlLoader;

use crate::dbm_dbc::{FKTable, InfoSchemaType, STable, SColumnDfn};

use self::yaml_rust::Yaml;
use serde::{Serialize, Serializer};
use std::fmt::{Formatter, Write};
use std::result::Result;

use crate::dbm_cfg::ConfigMap;
use crate::dbm_log::StatementLog;
use slog::Logger;
use std::cmp::Ordering;
use std::fmt;
use std::fmt::Display;
use serde::ser::SerializeSeq;
use std::convert::TryFrom;
use yaml_validator::Validate;
use crate::BASELINE;

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

#[derive(Debug, Clone)]
pub(crate) struct FileVersion {
    v: Version,
    pub s: String,
}

pub(crate) type SchemaTableMap = BTreeMap<FileVersion, (String, HashMap<String, YSchema>)>;
pub(crate) type VersionTableMap = BTreeMap<FileVersion, String>;

impl FileVersion {
    #[inline]
    pub(crate) fn new(vv: Version, key: String) -> Self {
        FileVersion { v: vv, s: key }
    }
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn from(key: &str) -> Result<Self, SemVerError> {
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

impl Serialize for FileVersion {
    fn serialize<S>(&self, serializer: S) -> Result<<S as Serializer>::Ok, <S as Serializer>::Error>
    where S: Serializer
    {
        serializer.serialize_str(&self.s)
    }
}

pub(crate) fn load_all(cfg: &ConfigMap, log: &Logger) -> Result<FilesMap, String> {
    let mut schema = FilesMap::new();
    let mut dir = cfg.get("source_dir");
    if dir.len() == 0 { dir.push_str("./"); }
    else if dir.as_bytes()[dir.len()-1] != b'/' { dir.push('/'); }


    let context = yaml_validator::Context::try_from(&crate::SCHEMA_YAML[..])
        .map_err(|e| format!("correct schema.yaml: context {}", e))?;

    load_dir(
        dir.as_str(), dir.as_str(),
        &mut schema,
        &FileFilters::new(cfg)?,
        cfg.get_bool("source_recur", true),
        &context,
        log,
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
    fn pks(&self, schema: &String, table: &String, target: &FileVersion) -> HashSet<String> {
        let mut pk = HashSet::new();
        for (v, (_, sks)) in &self.schemas {
            if v <= target {
                if let Some(fkst) = sks.get(schema) {
                    if let Some(tt) = fkst.tables.get(table) {
                        for cc in &tt.columns.list {
                            if cc.is_pk() {
                                pk.insert(cc.get_name());
                            }
                        }
                    }
                }
            }
        }
        pk
    }

    /// return sorted list of loaded versions
    pub fn all_versions(&self) -> BTreeSet<FileVersion>{
        let mut res = BTreeSet::new();
        self.schemas.keys().for_each(|f| {res.insert(f.clone());});
        self.pre.keys().for_each(|f| {res.insert(f.clone());});
        self.versions.keys().for_each(|f| {res.insert(f.clone());});
        self.re_runnable.keys().for_each(|f| {res.insert(f.clone());});
        res
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
    /// default version of xxx_deploy is a "target"
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
    root: &str,
    dir: &str,
    schema: &mut FilesMap,
    ff: &FileFilters,
    recursive: bool,
    context: &yaml_validator::Context,
    log: &Logger,
) -> Result<(), String> {
    match fs::read_dir(dir) {
        Ok(paths) => {
            for path in paths {
                match path {
                    Ok(p) => {
                        let fm = p.metadata().unwrap();
                        let fi = p.path().to_string_lossy().to_string();
                        if fm.is_dir() && recursive {
                            load_dir(root, fi.as_str(), schema, ff, recursive, context, log)?;
                        } else if fm.is_file() && fi.len() > 5 {
                            let file_name = if root.len() > 0 {
                                fi[root.len()..].to_string()
                            } else {
                                fi.clone()
                            };
                            // debug!(log, "parsing {} from {}", file_name, fi);
                            let starting_pos = match file_name.rfind("/") {
                                None => 0,
                                Some(i) => i + 1,
                            };
                            match file_name.as_bytes()[starting_pos] {
                                b'S' => {
                                    if let Some((vv, key)) =
                                        ff.schema_deploy.parse_type(&file_name, starting_pos)?
                                    {
                                        // todo read Schema in JSON format
                                        match load_yaml(fi.as_str(), context) {
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
                                b'a' | b'A' => {
                                    if file_name.ends_with(".sql") {
                                        let akey =  file_name[starting_pos..file_name.len() - 4].to_lowercase();
                                        schema.after.insert(akey, file_name);
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
pub(crate) struct FileFilter {
    /// None for any
    filter: Option<Version>,
    file_type: String,
}

impl FileFilter {
    #[inline]
    fn new(field: &str, cfg: &ConfigMap, file_type: &str) -> Result<Self, String> {
        let f = cfg.get_def(field, cfg.get_def("target", "*".into()));
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
    pub(crate) fn newt(file_type: &str) -> Self {
        FileFilter {
            filter: None,
            file_type: file_type.into(),
        }
    }

    #[inline]
    pub(crate) fn parse_type(
        &self,
        filename: &String,
        from_pos: usize,
    ) -> Result<Option<(Version, String)>, String> {
        if filename.to_lowercase().ends_with(&self.file_type) {
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
fn load_yaml(filename: &str, context: &yaml_validator::Context) -> Result<Yaml, String> {
    match fs::read_to_string(filename) {
        Ok(data) => match YamlLoader::load_from_str(data.as_str()) {
            Ok(mut y) => {
                let doc = y.remove(0);
                let yts = match context.get_schema("database") {
                    None => { return Err("correct schema.yaml: database".to_string()); }
                    Some(y) => y,
                };
                yts.validate(&context, &doc)
                    .map_err(|err| {format!("Schema [{}] validation error: {}", filename, err)})?;
                Ok(doc)
            },
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
    #[serde(with = "ytables")]
    pub tables: OrderedHashMap<YTable>,
    /// the table definition loaded from file
    #[serde(skip)]
    pub file: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct YTable {
    #[serde(rename = "tableName")]
    pub table_name: String,
    /// comments
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// transaction: -- single (default) OR table OR column OR retry (wrap to psql)
    #[serde(skip_serializing_if = "String::is_empty")]
    pub transaction: String,
    /// suffix on table create
    #[serde(skip_serializing_if = "String::is_empty")]
    pub sql: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub constraint: String,
    #[serde(with = "ycolumns")]
    pub columns: OrderedHashMap<YColumn>,
    #[serde(skip_serializing_if = "OrderedHashMap::is_empty")]
    pub triggers: OrderedHashMap<YTrig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub etl: Option<YEtl>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub owner: String,
    // #[serde(skip_serializing_if = "OrderedHashMap::is_empty")]
    // pub grant: OrderedHashMap<YTrig>,
    /// 0 - this table use no others, but other tables might use this one
    #[serde(skip)]
    pub weight: u8,
}

#[derive(Debug, Clone, Serialize)]
pub struct YEtl {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
}


/// wrapper for table to get a format as on lb
#[derive(Serialize)]
struct YtVO<'a> {
    /// createTable
    table: &'a YTable,
}

/// wrapper for columns to get a format as on lb
#[derive(Serialize)]
struct YcVO<'a> {
    column: &'a YColumn,
}

mod ycolumns {
    use serde::{Deserializer, Serializer};
    use crate::dbm_yml::{OrderedHashMap, YColumn, YcVO};
    use serde::ser::SerializeSeq;

    pub fn serialize<S>(columns: &OrderedHashMap<YColumn>, serializer: S) -> Result<<S as Serializer>::Ok, <S as Serializer>::Error> where
        S: Serializer
    {
        let mut seq = serializer.serialize_seq(Some(columns.len()))?;
        for c in &columns.list {
            let ref vo = YcVO {column: c};
            seq.serialize_element(vo)?;
        }
        seq.end()
    }

    #[allow(dead_code)]
    pub fn deserialize<'de, D>(_deserializer: D) -> Result<OrderedHashMap<YColumn>, D::Error> where D: Deserializer<'de> { unimplemented!() }

}

mod ytables {
    use serde::{Deserializer, Serializer};
    use crate::dbm_yml::{OrderedHashMap, YColumn, YtVO, YTable};
    use serde::ser::SerializeSeq;

    pub fn serialize<S>(tables: &OrderedHashMap<YTable>, serializer: S) -> Result<<S as Serializer>::Ok, <S as Serializer>::Error> where
        S: Serializer
    {
        let mut seq = serializer.serialize_seq(Some(tables.len()))?;
        for t in &tables.list {
            let ref vo = YtVO {table: t};
            seq.serialize_element(vo)?;
        }
        seq.end()
    }

    #[allow(dead_code)]
    pub fn deserialize<'de, D>(_deserializer: D) -> Result<OrderedHashMap<YColumn>, D::Error> where D: Deserializer<'de> { unimplemented!() }

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
    pub name: String,
    #[serde(rename = "type")]
    pub column_type: String,
    #[serde(rename = "defaultValue", skip_serializing_if = "Option::is_none")]
    pub default_value: Option<String>, //
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constraint: Option<YConstr>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub sql: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct YConstr {
    #[serde(rename = "primaryKey", skip_serializing_if = "Option::is_none")]
    pub primary_key: Option<bool>,
    pub nullable: bool,
    #[serde(rename = "foreignKey", skip_serializing_if = "Option::is_none")]
    pub foreign_key: Option<YForeignKey>,
}

#[derive(Debug, Clone, Serialize)]
pub struct YForeignKey {
    pub references: String, //fk_table
    #[serde(skip_serializing_if = "String::is_empty")]
    pub sql: String, //-- some SQL suffix on new FK create- on delete no action on update no action
}

#[derive(Debug, Clone, Serialize)]
pub struct YTrig {
    pub name: String,  //uniq_name_of_trigger
    pub event: String, // before update
    pub when: String,  //  for each row
    pub proc: String,  //  -- p()
}

impl YColumn {
    fn new(input: &Yaml) -> Self {
        let constraint = &input["constraint"];
        let foreign_key = &constraint["foreignKey"];
        let references = as_str_esc(foreign_key, "references");
        let (foreign_key, fk_set) = if references.len() == 0 {
            (None, false)
        } else {
            (
                Some(YForeignKey {
                    references,
                    sql: as_str_esc(foreign_key, "sql"),
                }),
                true,
            )
        };
        let primary_key = as_bool(constraint, "primaryKey", false);
        let nullable = as_bool(constraint, "nullable", true);
        let constraint = if primary_key || !nullable || fk_set {
            Some(YConstr {
                primary_key: if primary_key {Some(true)} else {None},
                nullable,
                foreign_key,
            })
        } else {
            None
        };
        YColumn {
            name: safe_sql(as_str_esc(input, "name")),
            column_type: as_str_esc(input, "type"),
            default_value: input["defaultValue"].as_str().map(|s| as_esc(s)),
            description: as_str_esc(input, "description"),
            sql: as_str_esc(input, "sql"),
            constraint,
        }
    }

    #[cfg(test)]
    pub(crate) fn newt(name: &str, ctype: &str, primary_key: bool, nullable: bool) -> Self {
        let constraint = if primary_key || !nullable {
            Some(YConstr {
                primary_key: if primary_key {Some(true)} else {None},
                nullable,
                foreign_key: None,
            })
        } else {
            None
        };
        YColumn {
            name: name.to_string(),
            column_type: ctype.to_string(),
            default_value: None,
            description: "".to_string(),
            sql: "".to_string(),
            constraint,
        }
    }

    pub(crate) fn is_pk(&self) -> bool {
        match &self.constraint {
            None => false,
            Some(c) => c.primary_key.unwrap_or(false),
        }
    }

    #[inline]
    fn column_def(
        &self,
        schema: &String,
        table_name: &String,
        file: &str,
    ) -> Result<SColumnDfn, String> {
        if self.column_type.len() == 0 {
            Err(format!(
                "Error column definition on table: {}.{} column {} file {}",
                schema, table_name, self.name, file
            ))
        } else {
            let c = self.constraint.clone();
            Ok(SColumnDfn {
                column_name: self.name.clone(),
                column_type: self.column_type.clone(),
                column_default: self.default_value.clone(),
                sql: Some(self.sql.trim().into()),
                pk: c.as_ref().map_or(false, |c| c.primary_key.unwrap_or(false)),
                nullable: c.as_ref().map_or(true, |c| c.nullable),
                fk: c.map_or(None, |cs| cs.foreign_key
                        .map_or(None, |fk| Some((fk.references.trim().into(), fk.sql.trim().into())))),
                sort_order: 0,
                column_comment: None
            })
        }
    }
}

impl YTrig {
    fn new(input: &Yaml) -> Self {
        YTrig {
            name: safe_sql(as_str_esc(input, "name")),
            event: as_str_esc(input, "event"),
            when: as_str_esc(input, "when"),
            proc: as_str_esc(input, "proc"),
        }
    }

    /// column defenition to SQL string
    #[inline]
    fn trig_def(&self, schema: &String, table_name: &String) -> Option<String> {
        // let proc = as_str_esc(input, "proc");
        if self.proc.len() > 0 {
            Some(format!(
                "CREATE TRIGGER {} {} ON {}.{} {} EXECUTE PROCEDURE {};",
                self.name, self.event, schema, table_name, self.when, self.proc
            ))
        } else {
            None
        }
    }
}

#[inline]
fn as_str_esc(input: &Yaml, field: &str) -> String {
    as_esc(as_str(input, field, "").as_str())
}

#[inline]
fn as_esc(val: &str) -> String {
    match val.find("--") {
        None => {
            if val.len() > 0 {
                format!("{}", val)
            } else {
                "".into()
            }
        }
        Some(i) => val[0..i].trim().into(),
    }
}

#[inline]
fn as_str(input: &Yaml, field: &str, def: &str) -> String {
    if input.is_null() {
        def.into()
    } else {
        match &input[field] {
            Yaml::Real(v) => v.to_string(),
            Yaml::Integer(v) => v.to_string(),
            Yaml::String(v) => v.to_string(),
            Yaml::Boolean(v) => v.to_string(),
            _ => def.into(),
        }
    }
}

#[inline]
fn as_stro(input: &Yaml, field: &str) -> Option<String> {
    if input.is_null() {
        None
    } else {
        match &input[field] {
            Yaml::String(v) => Some(v.to_string()),
            _ => None,
        }
    }
}

#[inline]
fn as_bool(input: &Yaml, field: &str, default: bool) -> bool {
    if input.is_null() {
        default
    } else {
        match &input[field] {
            Yaml::Integer(i) => i == &1i64,
            Yaml::String(s) => str2bool(s.as_str(), default),
            Yaml::Boolean(v) => *v,
            _ => default
        }
    }
}

#[inline]
pub(crate) fn str2bool(input: &str, default: bool) -> bool {
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
pub(crate) fn safe_sql(input: String) -> String {
    match input
        .chars()
        .position(|c| c == ' ' || c == '.' || c == ';' || c == '\n' || c == '\t')
    {
        None => input,
        Some(i) => input[0..i].into(),
    }
}

/// String key for BTreeMap, sorted by adding order
#[derive(Debug, Clone)]
pub struct OrderedHashMap<T: Named + Serialize> {
    map: BTreeMap<String, usize>,
    pub(crate) list: Vec<T>,
}

impl<T: Named + Serialize> OrderedHashMap<T> {
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
    pub(crate) fn get_mut(&mut self, key: &String) -> Option<&mut T> {
        match self.map.get_mut(key) {
            None => None,
            Some(id) => Some(&mut self.list[*id]),
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.list.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.map.len() == 0
    }
}

impl<T: Named + Serialize > Serialize for OrderedHashMap<T> {
    fn serialize<S>(&self, serializer: S) -> Result<<S as Serializer>::Ok, <S as Serializer>::Error> where
        S: Serializer, T: Named + Serialize {
            let mut seq = serializer.serialize_seq(Some(self.len()))?;
            for element in &self.list {
                seq.serialize_element(element)?;
            }
            seq.end()
        }
}

impl YTable {
    pub(crate) fn new(
        input: &Yaml,
        table_name: &str,
        file: Option<&String>,
        log: Option<&Logger>,
    ) -> Result<Self, String> {
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
                                    trace!(log, "+\n{:?}\n{:?}", yc, c);
                                }
                            }
                        }
                        if let Err(e) = columns.append(yc) {
                            return Err(format!(
                                "{} (column name) {}/{} on table: {}{}{}",
                                e,
                                i,
                                cls.len(),
                                table_name,
                                match file {
                                    None => "",
                                    Some(_) => ", found in file: ",
                                },
                                match file {
                                    None => "",
                                    Some(f) => f.as_str(),
                                },
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
                                    name,
                                    table_name,
                                    match file {
                                        None => "",
                                        Some(_) => ", found in file: ",
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
        let etl = &input["etl"];
        Ok(YTable {
            table_name: table_name.into(),
            description: as_str(input, "description", ""),
            transaction: as_str(input, "transaction", ""),
            sql: as_str_esc(input, "sql"),
            constraint: as_str_esc(input, "constraint"),
            columns,
            triggers,
            etl: if etl.is_null() {
                None
            } else {
                Some(YEtl {
                    file: as_stro(etl, "file"),
                    view: as_stro(etl, "view")
                })
            },
            owner: "".to_string(),
            weight: 0
        })
    }

    pub(crate) fn baseline_deploy(
        dbc: &mut InfoSchemaType,
        t: &mut Transaction,
        schema: &String,
        cfg: &ConfigMap,
        cause: &str,
        log: &Logger,
    ) -> Option<String> {
        debug!(log, "starting with self deploy {} for {}", BASELINE, cause);
        let mut baseline = crate::STATEMENT_LOG_TBL.clone();
        baseline.table_name = cfg.history_table_name();
        match baseline.deploy(dbc, t, &schema, cfg, BASELINE, log) {
            Ok(exec) => {
                if exec {
                    debug!(log, "self deploy of: {} complete", BASELINE);
                    if let Err(e) = StatementLog::new(cfg)
                        .instance('S', &BASELINE.into(), &"".into(), &BASELINE.into())
                        .persist(None, Some(t), &mut None)
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
                    let pks = ts.pks();
                    for dc in &self.columns.list {
                        if !ts.columns.contains_key(&dc.name) {
                            let def = dc.column_def(schema, &self.table_name, file)?;
                            append(format!(
                                "ALTER TABLE {}.{} ADD COLUMN {}",
                                schema, self.table_name, def.def(pks.is_some())
                            ).as_str(), &mut sql, cfg.is_retry());
                            let _ = ts.columns.insert(dc.get_name(), def);
                            exec = true;
                        }
                    }
                    if let Some(o) = &ts.owner {
                        if self.owner.len() > 0 && &self.owner != o {
                            append(format!("ALTER TABLE {}.{} OWNER TO {}",
                                schema, self.table_name, self.owner
                            ).as_str(), &mut sql, cfg.is_retry());
                        }
                    }
                    for dt in &self.triggers.list {
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
            // let owner = "";
            let mut columns = String::new();
            let mut st = STable {
                table_name: self.table_name.clone(),
                columns: HashMap::new(),
                fks: Default::default(),
                triggers: HashMap::new(),
                sort_order: 0,
                table_comment: None,
                owner: if self.owner.len() > 0 { Some(self.owner.clone()) } else { None },
            };
            for dc in &self.columns.list {
                let cd = dc.column_def(schema, &self.table_name, file)?;
                let _ = st.columns.insert(dc.get_name(), cd);
                self.comments(&mut comments, schema, &dc.name, &dc.description);
            }
            if columns.len() > 0 {
                let pks = st.pks();
                for dc in &self.columns.list {
                    if let Some(cd) = st.columns.get(dc.name.as_str()) {
                        columns.push_str(cd.def(pks.is_some()).as_str());
                        columns.push_str(", ");
                    }
                }
                if let Some(pks) = pks {
                    columns.push_str(pks.as_str());
                }
                if create_schema && schema.as_str() != "public" {
                    let _ = write!(sql, "CREATE SCHEMA IF NOT EXISTS {} ", schema);
                    if self.owner.len() > 0 {
                        let _ = write!(sql, "AUTHORIZATION {}", self.owner);
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
                if self.owner.len() > 0 {
                    append(format!(
                        "ALTER TABLE {}.{} OWNER TO {}",
                        schema, self.table_name, self.owner
                    ).as_str(),  &mut sql, cfg.is_retry());
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
                Err(e) => Err(format!("dry_run: [{}] {}", cfg.get("dry_run_to"), e)),
            }
        } else {
            deploy_log(&sql, log);
            if exec {
                debug!(log, "SQL> {}", sql);
                match db.batch_execute(sql.as_str()) {
                    Ok(()) => {
                        let _ = db.batch_execute(comments.as_str());
                    }
                    Err(e) => {
                        return Err(format!("DB execute [{}]: {}, source: {}", sql, e, file));
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
        target: &FileVersion,
        files: &FilesMap,
        dbc: &mut InfoSchemaType,
        db: &mut Transaction,
        schema: &String,
        cfg: &ConfigMap,
        file: &str,
        log: &Logger,
    ) -> Result<bool, String> {
        let mut sql = String::new();
        let mut fk_list = HashMap::new();
        if let Some(ss) = dbc.get(schema) {
            if let Some(ts) = ss.get(&self.table_name) {
                for dc in &self.columns.list {
                    if let Some(constraint) = &dc.constraint {
                        if let Some(fk) = &constraint.foreign_key {
                            let fk_table = &fk.references;
                            let (fk_schema, fk_table) = match fk_table.find(".") {
                                None => (schema.clone(), fk_table.to_string()),
                                Some(i) => {
                                    (fk_table[0..i].to_string(), fk_table[i + 1..].to_string())
                                }
                            };
                            // check for FK already in DB
                            if let Some(a) = dbc.get(schema) {
                                if let Some(b) = a.get(&self.table_name) {
                                    if let Some(_) = b.fks.get(&dc.name) {
                                        continue; // FK found as already created in DB, check the next column
                                    }
                                }
                            }
                            let fk_columns = files.pks(&fk_schema, &fk_table, target);
                            let key = format!("{}.{}", fk_schema, fk_table);
                            if fk_columns.len() > 0 {
                                fk_list.insert(key, FKTable {
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
        for ff in fk_list.values() {
            if let Some(mut ss) = dbc.get_mut(schema) {
                if let Some(mut ts) = ss.get_mut(&self.table_name) {
                    ts.fks.insert(ff.name.clone(), ff.clone());
                }
            }
            append(format!(
                "ALTER TABLE {}.{} ADD CONSTRAINT fk_{}_{}_{} FOREIGN KEY ({}) REFERENCES {}.{} ({}) {}",
                schema, self.table_name, schema, self.table_name, ff.table,
                ff.name, ff.schema, &ff.table, ff.columns(), ff.sql
            ).as_str(), &mut sql, cfg.is_retry());
        }

        if cfg.get_bool("dry_run", false) && exec {
            match StatementLog::dry_store(cfg.get_opt("dry_run_to"), sql, log) {
                Err(e) => Err(format!("dry_run: [{}] {}", cfg.get("dry_run_to"), e)),
                Ok(_) => Ok(false),
            }
        } else {
            if exec {
                if let Err(e) = db.batch_execute(sql.as_str()) {
                    return Err(format!("DB FK execute [{}]: {} source: {}", sql, e, file));
                }
            }
            Ok(exec)
        }
    }

    /// calc pseudo weight by constraints count
    // TODO replace to weight calculation: level from a top dictionary table
    pub(crate) fn pseudo_weight(&self) -> u8 {
        let mut w = 0;
        for c in &self.columns.list {
            if let Some(x) = &c.constraint {
                if x.foreign_key.is_some() {
                    w += 1;
                }
            }
        }
        w
    }

    pub(crate) fn is_table_transaction(&self) -> bool {
        self.transaction.as_str() == "table"
        || self.transaction.as_str() == "retry"
    }

}
//YTable

fn append(sql: &str, buff: &mut String, retry: bool) {
    if retry {
        buff.push_str(RPT1);
        buff.push_str(sql);
        buff.push_str(RPT2);
    } else {
        buff.push_str(sql);
        buff.push_str(";\n");
    }
}

const RPT1: &str = r#"DO
$do$
DECLARE
   lock_timeout CONSTANT text := '1000ms';
   max_attempts CONSTANT INT := 100;
   ddl_completed BOOLEAN := FALSE;
BEGIN

   PERFORM set_config('lock_timeout', lock_timeout, FALSE);

   FOR i IN 1..max_attempts LOOP
      BEGIN
         EXECUTE '"#;

const RPT2: &str = r#"';
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
"#;

#[cfg(debug_assertions)]
#[inline]
fn deploy_log(msg: &String, log: &Logger) {
    if msg.len() > 0 {
        debug!(log, "SQL>\n{}", msg);
    }
}

#[cfg(not(debug_assertions))]
#[allow(unused, warnings)]
#[inline]
fn deploy_log(msg: &String, log: &Logger) { }

impl YSchema {
    #[inline]
    fn schema_name(input: &Yaml) -> String {
        match input["schemaName"].as_str() {
            None => "public".into(),
            Some(s) => s.into(),
        }
    }

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
                let t = &t["table"];
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
                            let _ = self.tables.append(YTable::new(
                                t,
                                tn,
                                Some(&self.file),
                                Some(log),
                            )?);
                        }
                    }
                }
            }
        }
        Ok(())
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

    #[test]
    fn serialize_test() {
        let mut l: OrderedHashMap<YTrig> = OrderedHashMap::new();
        let _ = l.append(YTrig{
            name: "a".to_string(),
            event: "b".to_string(),
            when: "c".to_string(),
            proc: "d".to_string()
        });
        assert_eq!("[{'name':'a','event':'b','when':'c','proc':'d'}]".replace("'", "\""),
            serde_json::to_string(&l).unwrap());
    }

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


позволяет повесить ограничение not null, но не валидировать существующие записи сразу, а сделать это следующим шагом и без блокировок:

ALTER TABLE my_table ADD CONSTRAINT field_ecnrypted_not_null
    CHECK (field_encrypted IS NOT NULL) NOT VALID;

ALTER TABLE my_table VALIDATE CONSTRAINT field_ecnrypted_not_null;

*/
