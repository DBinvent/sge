extern crate yaml_rust;

use std::collections::HashMap;
use std::fs;

use postgres::Transaction;
use semver::Version;
use yaml_rust::YamlLoader;

use crate::dbm_dbc::{InfoSchemaType, STable, FKTable};

use self::yaml_rust::Yaml;
use serde::{Deserialize, Serialize};
use std::fmt::Write;
use std::result::Result;

use crate::dbm_log::StatementLog;
use clap::ArgMatches;
use lazy_static::lazy_static;
use slog::Logger;
use std::fmt;
use std::fmt::Display;

const STATEMENT_LOG_TABLE_YML: &str = "
tableName: rdbm_history
description: The schema deployment log
columns:
  - column:
    name: id
    type: serial
    constraint:
      primaryKey: true
      nullable: false
  - column:
    name: source
    type: varchar(250)
  - column:
    name: target
    type: varchar(250)
  - column:
    name: source_type
    type: char
    description: 'S: Schema, V: Version, R: Re-runnable'
  - column:
    name: version
    type: varchar(80)
  - column:
    name: created_on
    type: timestamp
    dfn: default current_timestamp()
  - column:
    name: checksum
    type: varchar(80)
  - column:
    name: scheduled_on
    type: timestamp
  - column:
    name: script_tag
    type: varchar(80)
  - column:
    name: catalog
    type: varchar(80)
  - column:
    name: executed_on
    type: timestamp
  - column:
    name: executed_by
    type: varchar(80)
  - column:
    name: ok
    type: bool
    dfn: not null default false
    description: Successful execution of the script
  - column:
    name: msg
    type: varchar(250)
  - column:
    name: took_sec
    type: bigint
";

lazy_static! {
    pub(crate) static ref STATEMENT_LOG_TBL: crate::dbm_yml::YTable =
        match YamlLoader::load_from_str(STATEMENT_LOG_TABLE_YML) {
            Ok(y) => {
                if y.len() > 0 {
                    crate::dbm_yml::YTable::new(
                        &y[0],
                        y[0]["tableName"].as_str().unwrap(),
                        true,
                        crate::BASELINE,
                    )
                    .unwrap()
                } else {
                    panic!("zero config");
                }
            }
            Err(e) => {
                panic!("wrong config: {}", e);
            }
        };
}

/// version is a [parse_type]
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FilesMap {
    /// version: schemaName : schema (with name)
    pub schemas: SchemaTableMap,
    /// version: file
    pub versions: VersionTableMap,
    /// version: file
    pub re_runnable: VersionTableMap,
    /// After scritps
    pub after: VersionTableMap,
}

pub(crate) type FileVersion = String;
pub(crate) type SchemaTableMap = HashMap<FileVersion, (String, HashMap<String, YSchema>)>;
pub(crate) type VersionTableMap = HashMap<FileVersion, String>;

pub(crate) fn load_all(cfg: &ConfigMap) -> Result<FilesMap, String> {
    let mut schema = FilesMap::new();
    load_dir(
        cfg.get("source_dir", "./").as_str(),
        &mut schema,
        cfg.get("schema_deploy", "*").as_str(),
        cfg.get("version_deploy", "*").as_str(),
        cfg.get("rr_deploy", "*").as_str(),
        cfg.get_bool("fail_safe", true),
        cfg.get_bool("source_recur", true),
    )?;
    Ok(schema)
}

impl FilesMap {
    fn new() -> Self {
        FilesMap {
            schemas: HashMap::new(),
            versions: HashMap::new(),
            re_runnable: HashMap::new(),
            after: HashMap::new(),
        }
    }

    /// looking for FKtable's PK
    #[inline]
    fn pks(&self, schema: &String, table: &String) -> String {
        let mut pk = String::new();
        for (_, (_, sks)) in &self.schemas {
            if let Some( fkst) = sks.get(schema) {
                if let Some(tt) = fkst.tables.get(table) {
                    for (cc, cy) in &tt.columns {
                        if tt.is_pk(cy) {
                            let _ = write!(pk, "{}{}", if pk.len()>0 {", "} else {""}, cc);
                        }
                    }
                }
            }
        }
        pk
    }
}

#[inline(never)]
fn load_dir(
    dir: &str,
    schema: &mut FilesMap,
    schema_deploy: &str,
    version_deploy: &str,
    rr_deploy: &str,
    fail_safe: bool,
    recursive: bool,
) -> Result<(), String> {
    match fs::read_dir(dir) {
        Ok(paths) => {
            for path in paths {
                match path {
                    Ok(p) => {
                        let fm = p.metadata().unwrap();
                        let file_name = p.path().to_string_lossy().to_string();
                        if fm.is_dir() && recursive {
                            load_dir(
                                file_name.as_str(),
                                schema,
                                schema_deploy,
                                version_deploy,
                                rr_deploy,
                                fail_safe,
                                recursive,
                            )?;
                        } else if fm.is_file() && file_name.len() > 5 {
                            let starting_pos = match file_name.rfind("/") {
                                None => 0,
                                Some(i) => i + 1,
                            };
                            match file_name.as_bytes()[starting_pos] {
                                b'S' => {
                                    let (key, ok) = parse_type(
                                        file_name.as_str(),
                                        schema_deploy,
                                        ".yaml",
                                        starting_pos,
                                        fail_safe,
                                    )?;
                                    if ok {
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
                                                            let _ = ss.append(s, fail_safe)?;
                                                        }
                                                    }
                                                    schema
                                                        .schemas
                                                        .insert(key, (file_name, schema_schemas));
                                                }
                                            },
                                            Err(e) => {
                                                if fail_safe {
                                                    return Err(format!("{}", e));
                                                }
                                            }
                                        }
                                    }
                                }
                                b'V' => {
                                    let (key, ok) = parse_type(
                                        file_name.as_str(),
                                        version_deploy,
                                        ".sql",
                                        starting_pos,
                                        fail_safe,
                                    )?;
                                    if ok {
                                        schema.versions.insert(key, file_name);
                                    }
                                }
                                b'R' => {
                                    let (key, ok) = parse_type(
                                        file_name.as_str(),
                                        rr_deploy,
                                        ".sql",
                                        starting_pos,
                                        fail_safe,
                                    )?;
                                    if ok {
                                        schema.re_runnable.insert(key, file_name);
                                    }
                                }
                                b'a' => {
                                    let (key, ok) = parse_type(
                                        file_name.as_str(),
                                        "*",
                                        ".sql",
                                        starting_pos,
                                        false,
                                    )?;
                                    if ok {
                                        schema.after.insert(key, file_name);
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

#[inline]
fn parse_type(
    filename: &str,
    filter: &str,
    filetype: &str,
    from_pos: usize,
    fail_safe: bool,
) -> Result<(String, bool), String> {
    if filename.ends_with(filetype) {
        match filename.find("__") {
            None => {
                if fail_safe {
                    Err(format!("unrecognizable version of file: {}", filename))
                } else {
                    Ok((filename[from_pos..filename.len() - 4].into(), true))
                }
            }
            Some(i) => {
                let ver = filename[from_pos + 1..i].replace("_", ".");
                let ok = cmp(ver.as_str(), filter, fail_safe)?;
                Ok((ver, ok))
            }
        }
    } else {
        Ok(("".into(), false))
    }
}

#[inline]
fn cmp(ver: &str, filter: &str, fail_safe: bool) -> Result<bool, String> {
    if filter.len() == 0 {
        Ok(false)
    } else if filter.as_bytes()[0] == b'*' {
        //|| (filter.len() == 1 && filter.as_bytes()[0] == b'^') {
        Ok(true)
    } else {
        let (fv, f_ok) = semver_parse(filter, fail_safe)?;
        let (vv, v_ok) = semver_parse(ver, fail_safe)?;
        Ok(f_ok && v_ok && fv.unwrap() >= vv.unwrap())
    }
}

#[inline]
fn semver_parse(input: &str, fail_safe: bool) -> Result<(Option<Version>, bool), String> {
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
        Ok(v) => Ok((Some(v), true)),
        Err(e) => {
            if fail_safe {
                Err(format!("version parsing: {} error: {}", input, e))
            } else {
                Ok((None, false))
            }
        }
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
    pub schema_name: String,
    pub owner: String,
    /// tableName: table(with name)
    pub tables: HashMap<String, YTable>,
    /// the table definition loaded from file
    pub file: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct YTable {
    pub table_name: String,
    /// comments
    pub description: String,
    /// suffix on table create
    pub sql: String,
    pub unlogged: bool,
    #[serde(skip_serializing)]
    pub columns: HashMap<String, Yaml>,
    #[serde(skip_serializing)]
    pub triggers: HashMap<String, Yaml>,
}

#[inline]
pub(crate) fn as_str_esc(input: &Yaml, field: &str) -> String {
    let val = as_str_def(input, field, "");
    match val.find("--") {
        None => val,
        Some(i) => val[0..i].into(),
    }
}

#[inline]
pub(crate) fn as_str(input: &Yaml, field: &str) -> String {
    as_str_def(input, field, "")
}

#[inline]
pub(crate) fn as_str_def(input: &Yaml, field: &str, def: &str) -> String {
    match input[field].as_str() {
        None => def.into(),
        Some(s) => s.into(),
    }
}

#[inline]
pub(crate) fn as_bool(input: &Yaml, field: &str, default: bool) -> bool {
    match input[field].as_str() {
        None => default,
        Some(s) => str2bool(s, default),
    }
}

#[inline]
fn str2bool(input: &str, default: bool) -> bool {
    if input.len() == 0 {
        default
    } else {
        input == "true" || input == "yes" || input == "TRUE" || input == "YES" || input == "Y"
            || input == "ok" || input == "OK" || input == "ON" || input == "on"
            || input == "T" || input == "+" || input == "1"
    }
}

impl YTable {
    pub(crate) fn new(
        input: &Yaml,
        table_name: &str,
        fail_safe: bool,
        file: &str,
    ) -> Result<Self, String> {
        let mut columns = HashMap::new();
        if let Some(cls) = input["columns"].as_vec() {
            for c in cls {
                if let Some(name) = c["name"].as_str() {
                    if let Some(_) = columns.insert(name.into(), c.clone()) {
                        if fail_safe {
                            return Err(format!(
                                "Duplicate column name: {} on table: {}, found in file: {}",
                                name, table_name, file
                            ));
                        };
                    };
                }
            }
        }
        let mut triggers = HashMap::new();
        if let Some(cls) = input["triggers"].as_vec() {
            for c in cls {
                if let Some(name) = c["name"].as_str() {
                    if let Some(_) = triggers.insert(name.into(), c.clone()) {
                        if fail_safe {
                            return Err(format!(
                                "Duplicate trigger name: {} on table: {}, found in file: {}",
                                name, table_name, file
                            ));
                        };
                    };
                }
            }
        }
        Ok(YTable {
            table_name: table_name.into(),
            description: as_str(input, "description"),
            sql: as_str_esc(input, "sql"),
            unlogged: as_bool(input, "unlogged", false),
            columns,
            triggers,
        })
    }

    /// build a create or alter sql
    #[allow(unused_mut)]
    pub(crate) fn deploy(&self, dbc: &mut InfoSchemaType, db: &mut Transaction,
        schema: &String, cfg: &ConfigMap, file: &str, log: &Logger,
    ) -> Result<bool, String> {
        let mut sql = String::new();
        let mut comments = String::new();
        let mut exec = false;
        let (create_schema, create_table) = match dbc.get_mut(schema) {
            None => (true, true),
            Some(mut ss) => match ss.get_mut(&self.table_name) {
                None => (false, true),
                Some(mut ts) => {
                    for (dcn, dc) in &self.columns {
                        if !ts.columns.contains_key(dcn) {
                            let def = self.column_def(dcn, dc, file)?;
                            let _ = write!(
                                sql,
                                "ALTER TABLE {}.{} ADD COLUMN {};",
                                schema, self.table_name, def
                            );
                            let _ = ts.columns.insert(dcn.clone(), def);
                            exec = true;
                        }
                    }
                    for (dcn, dt) in &self.triggers {
                        if !ts.triggers.contains_key(dcn) {
                            if let Some(def) = self.trig_def(schema, dcn, dt) {
                                let _ = writeln!(sql, "{}", def);
                                let _ = ts.triggers.insert(dcn.clone(), def);
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
            for (dcn, dc) in &self.columns {
                let cd = self.column_def(dcn, dc, file)?;
                let _ = write!(columns, "{}, ", cd);
                let _ = st.columns.insert(dcn.clone(), cd);
                self.comments(&mut comments, schema, dcn, dc);
            }
            if columns.len() > 0 {
                if create_schema {
                    let _ = write!(sql, "CREATE SCHEMA IF NOT EXISTS {} ", schema);
                    if owner.len() > 0 {
                        let _ = write!(sql, "AUTHORIZATION {}", owner);
                    }
                    let _ = write!(sql, ";\n");
                }
                columns.remove(columns.rfind(",").unwrap());
                let _ = writeln!(
                    sql,
                    "CREATE TABLE {}.{} ({}) {}; \n",
                    schema, self.table_name, columns, self.sql
                );
                if owner.len() > 0 {
                    let _ = writeln!(
                        sql,
                        "ALTER TABLE {}.{} OWNER TO {}; \n",
                        schema, self.table_name, owner
                    );
                }
            }
            for (dcn, dt) in &self.triggers {
                if let Some(td) = self.trig_def(schema, dcn, dt) {
                    let _ = writeln!(sql, "{}\n", td);
                    st.triggers.insert(dcn.clone(), td);
                }
            }
            dbc.get_mut(schema)
                .unwrap()
                .insert(self.table_name.clone(), st);
            exec = true;
        }
        if comments.len() > 0 {
            let _ = writeln!(sql, "{}\n", comments);
        }
        if self.description.len() > 0 {
            let _ = writeln!(
                sql,
                "COMMENT ON TABLE {}.{} IS '{}'; \n",
                schema, self.table_name, self.description
            );
        }

        if cfg.get_bool("dry_run", false) {
            match StatementLog::dry_store(cfg.get_opt("dry_run_to"), sql, log) {
                Ok(_) => Ok(false),
                Err(e) => Err(format!("dry_run: [{}] {}", cfg.get("dry_run_to", ""), e)),
            }
        } else {
            if exec {
                if let Err(e) = db.batch_execute(sql.as_str()) {
                    return Err(format!("DB execute [{}]: {}", sql, e));
                }
            }
            Ok(exec)
        }
    }

    //YTable

    /// column defenition to SQL string
    #[inline]
    fn column_def(&self, column_name: &String, input: &Yaml, file: &str) -> Result<String, String> {
        match input["type"].as_str() {
            None => Err(format!(
                "Error column definition on table: {} column {} file {}",
                self.table_name, column_name, file
            )),
            Some(t) => {
                if t.len() == 0 {
                    Err(format!(
                        "Error column definition - empty type, on table: {} column {} file {}",
                        self.table_name, column_name, file
                    ))
                } else {
                    let mut cs = String::new();
                    let constraint = &input["constraint"];
                    if !constraint.is_null() {
                        if let Some(pk) = constraint["primaryKey"].as_bool() {
                            if pk {
                                let _ = write!(cs, " primary key");
                            }
                        }
                        if let Some(nb) = constraint["nullable"].as_bool() {
                            if !nb {
                                let _ = write!(cs, " not null");
                            }
                        }
                    }
                    Ok(format!("{} {} {} {}", column_name, t, cs, as_str_esc(input, "sql")))
                }
            }
        }
    }

    #[inline]
    fn is_pk(&self, input: &Yaml) -> bool {
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
    fn comments(&self, sql: &mut String, schema: &String, column_name: &String, input: &Yaml) {
        if let Some(t) = input["description"].as_str() {
            if t.len() > 0 {
                let _ = writeln!(
                    sql,
                    "COMMENT ON COLUMN {}.{}.{} IS '{}';",
                    schema, self.table_name, column_name, self.description
                );
            }
        }
    }

    /// column defenition to SQL string
    #[inline]
    fn trig_def(&self, schema: &String, trig_name: &String, input: &Yaml) -> Option<String> {
        let proc = as_str(input, "proc");
        if proc.len() > 0 {
            Some(format!(
                "CREATE TRIGGER {}.{} {} ON {}.{} {} EXECUTE PROCEDURE {};",
                schema,
                trig_name,
                as_str_def(input, "event", "BEFORE UPDATE"),
                schema,
                self.table_name,
                as_str_def(input, "when", "FOR EACH ROW"),
                proc
            ))
        } else {
            None
        }
    }

    /// build a create or alter sql
    #[allow(unused, unused_mut)]
    pub(crate) fn deploy_fk(&self, yschemas: &FilesMap, dbc: &mut InfoSchemaType, db: &mut Transaction,
        schema: &String, cfg: &ConfigMap, _file: &str, log: &Logger,
    ) -> Result<bool, String> {
        let mut sql = String::new();
        let mut fk_list = Vec::new();
        if let Some(ss) = dbc.get(schema) {
            if let Some(ts) = ss.get(&self.table_name) {
                for (dcn, dc) in &self.columns {
                    if ts.columns.contains_key(dcn) {
                        let constraint = &dc["constraint"];
                        if !constraint.is_null() {
                            let fk = &constraint["foreignKey"];
                            if !fk.is_null() {
                                if let Some(fk_table) = constraint["references"].as_str() {
                                    let (fk_schema, fk_table) = match fk_table.find(".") {
                                        None => (schema.clone(), fk_table.to_string()),
                                        Some(i) => (fk_table[0..i].to_string(), fk_table[i+1..].to_string())
                                    };
                                    // check for absent in DB
                                    if let Some(a) = dbc.get(schema) {
                                        if let Some(b) = a.get(&self.table_name) {
                                            if let Some(_) = b.fks.get(dcn) {
                                                continue; // FK found in DB, to the next column
                                            }
                                        }
                                    }
                                    let fk_columns = yschemas.pks(&fk_schema, &fk_table);
                                    if fk_columns.len() > 0 {
                                        fk_list.push(FKTable {
                                            column: fk_columns,
                                            name: dcn.clone(),
                                            schema: fk_schema,
                                            table: fk_table,
                                            sql: as_str_esc(constraint, "sql")
                                        });
                                    }
                                }
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

        if cfg.get_bool("dry_run", false) {
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

    #[inline]
    fn new(input: &Yaml, file: &String) -> Self {
        YSchema {
            schema_name: YSchema::schema_name(input),
            owner: as_str(input, "schemaName"),
            tables: HashMap::new(),
            file: file.clone(),
        }
    }

    #[inline]
    fn append(&mut self, input: &Yaml, fail_safe: bool) -> Result<(), String> {
        if let Some(tbls) = input["tables"].as_vec() {
            for t in tbls {
                match t["tableName"].as_str() {
                    None => {
                        return Err(format!("no table name set in file: {}", self.file));
                    }
                    Some(tn) => {
                        if self.tables.contains_key(tn) && fail_safe {
                            return Err(format!(
                                "duplicate table definition: {} found in file: {}",
                                tn, self.file
                            ));
                        } else {
                            self.tables.insert(
                                tn.into(),
                                YTable::new(t, tn, fail_safe, self.file.as_str())?,
                            );
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
    pub(crate) fn get(&self, param: &str, default: &str) -> String {
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
    pub(crate) fn get_opt(&self, param: &str) -> Option<String> {
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
        None //Err(format!("Param not exists: {}", param))
    }

    #[inline]
    pub(crate) fn get_pow(&self, content: &mut String, prefix: &str, param: &str, amp: &str) {
        if let Some(v) = self.get_opt(format!("{}{}", prefix, param).as_str()) {
            let _ = write!(
                content,
                "{}{}={}",
                if content.len() > 0 { amp } else { "" },
                param,
                v
            );
        }
    }

    #[inline]
    pub(crate) fn get_bool(&self, param: &str, default: bool) -> bool {
        str2bool(
            self.get(param, format!("{}", default).as_str()).as_str(),
            default,
        )
    }

    /// find the target database name for log storage, use a database from connection if not set
    /// use 'dbname'
    #[inline]
    pub(crate) fn target(&self) -> String {
        self.get("db_target", "")
    }

    #[inline]
    pub(crate) fn schema(&self) -> String {
        self.get("schema", "public")
    }

    #[inline]
    pub(crate) fn history_table_name(&self) -> String {
        self.get("table", STATEMENT_LOG_TBL.table_name.as_str())
    }

    #[inline]
    pub fn is_pgsrv(&self) -> bool {
        let mut result = false;
        if let Some(_) = self.get_opt("db_password") {
            result = true;
        }
        if let Some(_) = self.get_opt("db_sslmode") {
            result = true
        }
        result
    }

    #[inline]
    pub fn is_ssl(&self) -> bool {
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
    fn statement_deser_test() {
        let mut schema = FilesMap::new();
        match load_dir(&"./tests", &mut schema, "*", "1.1", "1.1", true, true) {
            Ok(_) => {
                assert_eq!(schema.schemas.len(), 1);
                // assert_eq!(schema.schemas.iter().next().unwrap().1.tables.len(), 1);
                assert_eq!(schema.re_runnable.len(), 1);
                assert_eq!(schema.versions.len(), 1);
                assert!(schema.after.len() >= 1);
                println!("{:?}", schema);
            }
            Err(e) => {
                assert!(false, "{}", e);
            }
        }
    }

    #[test]
    fn statementlog_deser_test() {
        assert!(STATEMENT_LOG_TBL.table_name.len() > 1);
        assert!(STATEMENT_LOG_TBL.description.len() > 10);
        assert!(STATEMENT_LOG_TBL.columns.len() > 10);
        assert!(crate::CONFIG_YAML["args"].as_vec().unwrap().len() > 10);
    }

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
}
