extern crate yaml_rust;

use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::fs;
use std::fs::{DirEntry, Metadata, ReadDir};
use std::io::Error;

use postgres::{Client, Transaction};
use semver::{SemVerError, Version};
use yaml_rust::YamlLoader;

use crate::dbm_dbc::{IS_TYPE, STable};
use crate::dbm_log::{LS_TYPE, StatementLog};

use self::yaml_rust::{ScanError, Yaml};
use std::fmt::Write;
use std::result::Result;
use serde::Deserialize;

use lazy_static::lazy_static;
use std::fmt::Display;
use std::fmt;
use slog::Logger;


const STATEMENT_LOG_TABLE_YML: &str = "
tableName: rdbm_history
description: The schema deployment log
columns:
  - column:
    name: id
    type: serial
  - column:
    name: source
    type: varchar(250)
  - column:
    name: database
    type: varchar(150)
  - column:
    name: schemas_regxp
    type: varchar(250)
  - column:
    name: source_type
    type: char
    description: 'S: Schema, V: Version, R: Re-runnable'
  - column:
    name: version
    type: varchar(50)
  - column:
    name: created
    type: timestamp
    dfn: default current_timestamp()
  - column:
    name: checksum
    type: varchar(50)
  - column:
    name: scheduled_on
    type: timestamp
  - column:
    name: script_ver
    type: varchar(150)
  - column:
    name: executed_on
    type: timestamp
  - column:
    name: executed_by
    type: varchar(50)
  - column:
    name: ok
    type: bool
    dfn: not null default false
    description: Successful execution of the script
  - column:
    name: msg
    type: varchar(250)
  - column:
    name: applied_by
    type: bigint
  - column:
    name: applied_from
    type: bigint
  - column:
    name: applied_to
    type: bigint
"
;


/// default configurations with a description
const CONFIG_DEF: &str = "
(params: {
\"db_name\": (val: \"postgres\", dsr: \"Postgres database\"),
\"db_host\": (val: \"/var/run/postgresql\", dsr: \"Postgres host\"),
\"db_port\": (val: \"5432\", dsr: \"Postgres port\"),
\"db_user\": (val: \"postgres\", dsr: \"\"),
\"db_password\": (val: \"\", dsr: \"\"),
\"db_sslmode\": (val: \"verify-full\", dsr: \"\"),
\"db_sslrootcert\": (val: \"\", dsr: \"/path/to/server-ca.crt\"),
\"db_sslcert\": (val: \"\", dsr: \"/path/to/client.crt\"),
\"db_sslkey\": (val: \"\", dsr: \"/path/to/client.key\"),
\"logdb_name\": (val: \"postgres\", dsr: \"Postgres database\"),
\"logdb_host\": (val: \"/var/run/postgresql\", dsr: \"Postgres host\"),
\"logdb_port\": (val: \"5432\", dsr: \"Postgres port\"),
\"logdb_user\": (val: \"postgres\", dsr: \"\"),
\"logdb_password\": (val: \"\", dsr: \" \"),
\"logdb_sslmode\": (val: \"verify-full\", dsr: \"\"),
\"logdb_sslrootcert\": (val: \"\", dsr: \"/path/to/server-ca.crt\"),
\"logdb_sslcert\": (val: \"\", dsr: \"/path/to/client.crt\"),
\"logdb_sslkey\": (val: \"\", dsr: \"/path/to/client.key\"),
\"ignore_order\": (val: \"false\", dsr: \"Perform Versions order execution\"),
\"schema_regxp\": (val: \"*\", dsr: \"Scan schemas\"),
\"schema_deploy\": (val: \"*\", dsr: \"Perform Deploy (S)chema files, i.e. target schema files to deploy, up to the target version (* means all)\"),
\"version_deploy\": (val: \"*\", dsr: \"Perform Deploy (V)ersion files up to the target version (* means all)\"),
\"version_checksum\": (val: \"false\", dsr: \"Fail on deployed scripts version checksum mismatch\"),
\"rr_deploy\": (val: \"*\", dsr: \"Perform Deploy (R)e-runable scripts files up to the target version (* means all)\"),
\"schema\": (val: \"public\", dsr: \"deployment history statement_log location\"),
\"source_dir\": (val: \".\", dsr: \"Directory scripts location\"),
\"source_recur\": (val: \"true\", dsr: \"Scan Directory recursively\"),
\"fail_safe\": (val: \"false\", dsr: \"Fail the app if applied file different the file provided\"),
\"dry_run\": (val: \"false\", dsr: \"generate, but do not run schema, also log all operations\"),
})"
;

lazy_static! {
	pub(crate) static ref STATEMENT_LOG_TBL: crate::dbm_yml::YTable = match YamlLoader::load_from_str(STATEMENT_LOG_TABLE_YML) {
		Ok(y) => {
			if y.len() > 0 {
				crate::dbm_yml::YTable::new(&y[0], y[0]["tableName"].as_str().unwrap(), true, crate::BASELINE).unwrap()
			} else { panic!(format!("zero config")); }
		},
		Err(e) => {panic!(format!("wrong config: {}", e));}
	};

	pub(crate) static ref CONFIG_MAP: ConfigDefMap = match ron::de::from_str(CONFIG_DEF) {
		Ok(c) => c,
		Err(e) => { panic!(format!("wrong config: {}", e)); }
	};
}

pub fn history_table_name() -> String {
    STATEMENT_LOG_TBL.table_name.clone()
}

/// version is a [parse_type]
#[derive(Debug, Clone)]
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
pub(crate) type SchemaTableMap = HashMap<FileVersion, HashMap<String, YSchema>>;
pub(crate) type VersionTableMap = HashMap<FileVersion, String>;

pub(crate) fn load_all(cfg: &ConfigMap) -> Result<FilesMap, String> {
    let mut schema = FilesMap::new();
    load_dir(cfg.get_opt("source_dir")?.as_str(), &mut schema,
             cfg.get("version_deploy", "*").as_str(),
             cfg.get("rr_deploy", "*").as_str(),
             cfg.get("schema_deploy", "*").as_str(),
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
}

#[inline(never)]
fn load_dir(dir: &str, schema: &mut FilesMap,
            schema_deploy: &str, version_deploy: &str, rr_deploy: &str,
            fail_safe: bool, recursive: bool) -> Result<(), String> {
    match fs::read_dir(dir) {
        Ok(paths) => {
            for path in paths {
                match path {
                    Ok(p) => {
                        let fm = p.metadata().unwrap();
                        let file_name = p.path().to_string_lossy().to_string();
                        if fm.is_dir() && recursive {
                            load_dir(file_name.as_str() //p.path().to_str().unwrap()
                                     , schema, schema_deploy, version_deploy, rr_deploy, fail_safe, recursive);
                        } else if fm.is_file() && file_name.len() > 5 {
                            let starting_pos = match file_name.rfind("/") {
                                None => 0,
                                Some(i) => i + 1,
                            };
                            match file_name.as_bytes()[starting_pos] {
                                b'S' => {
                                    let (key, ok) = parse_type(file_name.as_str(), schema_deploy, ".yaml", starting_pos, fail_safe)?;
                                    if ok {
                                        match load(file_name.as_str()) {
                                            Ok(y) => {
                                                match y["database"].as_vec() {
                                                    None => {}
                                                    Some(schemas) => {
                                                        let mut schema_schemas = HashMap::new();

                                                        for s in schemas {
                                                            let ysn = YSchema::schemaName(s);
                                                            if !schema_schemas.contains_key(&ysn) {
                                                                schema_schemas.insert(ysn.clone(), YSchema::new(&s, &file_name));
                                                            }
                                                            if let Some(ss) = schema_schemas.get_mut(&ysn) {
                                                                let _ = ss.append(s, fail_safe)?;
                                                            }
                                                        }

                                                        schema.schemas.insert(key, schema_schemas);
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                if fail_safe {
                                                    return Err(format!("{}", e));
                                                }
                                            }
                                        }
                                    }

                                }
                                b'V' => {
                                    let (key, ok) = parse_type(file_name.as_str(), version_deploy, ".sql", starting_pos, fail_safe)?;
                                    if ok {
                                        schema.versions.insert(key, file_name);
                                    }
                                }
                                b'R' => {
                                    let (key, ok) = parse_type(file_name.as_str(), rr_deploy, ".sql", starting_pos, fail_safe)?;
                                    if ok {
                                        schema.re_runnable.insert(key, file_name);
                                    }
                                }
                                b'a' => {
                                    let (key, ok) = parse_type(file_name.as_str(), "*", ".sql", starting_pos, false)?;
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
            };
            Ok(())
        }
        Err(e) => {
            Err(format!("read error: {}", e))
        }
    }
}

#[inline]
fn parse_type(filename: &str, filter: &str, filetype: &str, from_pos: usize, fail_safe: bool) -> Result<(String, bool), String> {
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
    } else if filter.as_bytes()[0] == b'*' { //|| (filter.len() == 1 && filter.as_bytes()[0] == b'^') {
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
    let input = if fd < ld { input.to_string() } else if fd == 0 { format!("{}.0.0", input) } else { format!("{}.0", input) };
    match semver::Version::parse(input.as_str()) {
        Ok(v) => Ok((Some(v), true)),
        Err(e) => {
            if fail_safe {
                Err((format!("version parsing: {} error: {}", input, e)))
            } else {
                Ok((None, false))
            }
        }
    }
}

#[inline]
fn load(filename: &str) -> Result<Yaml, String> {
    match fs::read_to_string(filename) {
        Ok(data) => {
            match YamlLoader::load_from_str(data.as_str()) {
                Ok(mut y) => {
                    Ok(y.remove(0))
                }
                Err(e) => Err(format!("parsing error: {} of file: {}", e, filename))
            }
        }
        Err(e) => {
            Err(format!("read error: {}", e))
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct YSchema {
    pub schema_name: String,
    pub owner: String,
    /// tableName: table(with name)
    pub tables: HashMap<String, YTable>,
    /// the table definition loaded from file
    pub file: String,
}

#[derive(Debug, Clone)]
pub(crate) struct YTable {
    pub table_name: String,
    /// comments
    pub description: String,
    /// suffix on table create
    pub sql: String,
    pub unlogged: bool,
    pub columns: HashMap<String, Yaml>,
    pub triggers: HashMap<String, Yaml>,
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
        Some(s) => { if default { s != "false" } else { s != "true" } }
    }
}

impl YTable {
    pub(crate) fn new(input: &Yaml, table_name: &str, fail_safe: bool, file: &str) -> Result<Self, String> {
        let mut columns = HashMap::new();
        if let Some(cls) = input["columns"].as_vec() {
            for c in cls {
                if let Some(name) = c["name"].as_str() {
                    if let Some(_) = columns.insert(name.into(), c.clone()) {
                        if fail_safe { return Err(format!("Duplicate column name: {} on table: {}, found in file: {}", name, table_name, file)); };
                    };
                }
            }
        }
        let mut triggers = HashMap::new();
        if let Some(cls) = input["triggers"].as_vec() {
            for c in cls {
                if let Some(name) = c["name"].as_str() {
                    if let Some(_) = triggers.insert(name.into(), c.clone()) {
                        if fail_safe { return Err(format!("Duplicate trigger name: {} on table: {}, found in file: {}", name, table_name, file)); };
                    };
                }
            }
        }
        Ok(YTable {
            table_name: table_name.into(),
            description: as_str(input, "description"),
            sql: as_str(input, "sql"),
            unlogged: as_bool(input, "unlogged", false),
            columns,
            triggers,
        })
    }

    /// build a create or alter sql
    pub(crate) fn deploy(&self, dbc: &mut IS_TYPE, db: &mut Transaction, schema: &String, cfg: &ConfigMap, file: &str, log: &Logger) -> Result<(), String> {
        let mut sql = String::new();
        let mut comments = String::new();
        let (create_schema, create_table) = match dbc.get(schema) {
            None => (true, true),
            Some(ss) => {
                match ss.get(&self.table_name) {
                    None => (false, true),
                    Some(ts) => {
                        for (dcn, dc) in &self.columns {
                            if !ts.columns.contains_key(dcn) {
                                write!(sql, "ALTER TABLE {}.{} ADD COLUMN {};", schema, self.table_name, self.column_def(dcn, dc, file)?);
                            }
                        }
                        for (dcn, dt) in &self.triggers {
                            if !ts.triggers.contains_key(dcn) {
                                writeln!(sql, "{}", self.trig_def(schema, dcn, dt));
                            }
                        }
                        (false, false)
                    }
                }
            }
        };
        // TODO FIX NOW update dbc
        // TODO ADD FK
        if create_table {
            let owner = "";
            let mut columns = String::new();
            for (dcn, dc) in &self.columns {
                write!(sql, "{}, ", self.column_def(dcn, dc, file)?);
                self.comments(&mut comments, schema, dcn, dc);
            }
            if columns.len() > 0 {
                if create_schema {
                    write!(sql, "CREATE SCHEMA IF NOT EXISTS {} ", schema);
                    if owner.len() > 0 {
                        write!(sql, "AUTHORIZATION {}", owner);
                    }
                    write!(sql, ";\n");
                }
                columns.remove(columns.rfind(",").unwrap());
                writeln!(sql, "CREATE TABLE {}.{} ({}) {};", schema, self.table_name, columns, self.sql);
                if owner.len() > 0 {
                    writeln!(sql, "ALTER TABLE {}.{} OWNER TO {};", schema, self.table_name, owner);
                }
            }
            for (dcn, dt) in &self.triggers {
                // add trigger
                writeln!(sql, "{}", self.trig_def(schema, dcn, dt));
            }
        }
        if comments.len() > 0 {
            writeln!(sql, "{}", comments);
        }
        if self.description.len() > 0 {
            writeln!(sql, "COMMENT ON TABLE {}.{} IS '{}';", schema, self.table_name, self.description);
        }

        let dry_run = cfg.get_bool("dry_run", false);
        if dry_run {
            match cfg.get_opt("dry_run_to") {
                Ok(fto) => {
                    fs::write(fto, sql);
                }
                Err(_) => {
                    info!(log, "{}", sql);
                }
            }
            Ok(())
        } else {
            if let Err(e) = db.batch_execute(sql.as_str()) {
                Err(format!("DB execute: {}", e))
            } else { Ok(()) }
         }
    }

    /// column defenition to SQL string
    #[inline]
    fn column_def(&self, column_name: &String, input: &Yaml, file: &str) -> Result<String, String> {
        match input["type"].as_str() {
            None => Err(format!("Error column definition on table: {} column {} file {}", self.table_name, column_name, file)),
            Some(t) => {
                if t.len() == 0 {
                    Err(format!("Error column definition - empty type, on table: {} column {} file {}", self.table_name, column_name, file))
                } else {
                    Ok(format!("{} {}", column_name, t))
                }
            }
        }
    }
    /// column defenition to SQL string
    #[inline]
    fn comments(&self, sql: &mut String, schema: &String, column_name: &String, input: &Yaml) {
        if let Some(t) = input["description"].as_str() {
            if t.len() > 0 {
                writeln!(sql, "COMMENT ON COLUMN {}.{}.{} IS '{}';", schema, self.table_name, column_name, self.description);
            }
        }
    }

    /// column defenition to SQL string
    #[inline]
    fn trig_def(&self, schema: &String, trig_name: &String, input: &Yaml) -> String {
        format!("CREATE TRIGGER {}.{} {} ON {}.{} {} EXECUTE PROCEDURE {};",
                schema, trig_name, as_str_def(input, "event", "BEFORE UPDATE"), schema, self.table_name,
                as_str_def(input, "when", "FOR EACH ROW"), as_str(input, "proc") )
    }
}
//YTable

impl YSchema {
    #[inline]
    fn schemaName(input: &Yaml) -> String {
        match input["schemaName"].as_str() {
            None => "public".into(),
            Some(s) => s.into(),
        }
    }

    #[inline]
    fn new(input: &Yaml, file: &String) -> Self {
        YSchema {
            schema_name: YSchema::schemaName(input),
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
                            return Err(format!("duplicate table definition: {} found in file: {}", tn, self.file));
                        } else {
                            self.tables.insert(tn.into(), YTable::new(t, tn, fail_safe, self.file.as_str())?);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct ConfigMap {
	params: HashMap<String, String>,
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

impl ConfigMap {
    #[inline]
	pub fn new(param: &str, value: &str) -> Self {
		let mut params = HashMap::new();
		params.insert(param.to_string(),   value.into() );
		ConfigMap{
			params
		}
	}
    #[inline]
	pub fn set(&mut self, param: &str, value: &str)  {
		let _ = self.params.insert(param.into(), value.into());
	}

    #[inline]
	pub fn get(&self, param: &str, default: &str) -> String {
		match self.params.get(param) {
			None => {
				match CONFIG_MAP.params.get(param) {
					None => default.into(),
					Some(x) => x.val.clone()
				}
			}
			Some(x) => x.clone(),
		}
	}

    #[inline]
	pub fn get_opt(&self, param: &str) -> Result<String, String> {
		match self.params.get(param) {
			None => {
				match CONFIG_MAP.params.get(param) {
					None => Err(format!("Param not exists: {}", param)),
					Some(x) => Ok(x.val.clone())
				}
			}
			Some(x) => Ok(x.clone()),
		}
	}

    #[inline]
	pub fn get_po(&self, prefix: &str, param: &str) -> Result<String, String> {
		let key = format!("{}{}", prefix, param);
		match self.params.get(key.as_str()) {
			None => {
				match CONFIG_MAP.params.get(key.as_str()) {
					None => Err(format!("Param not exists: {}", key)),
					Some(x) => Ok(x.val.clone())
				}
			}
			Some(x) => Ok(x.clone()),
		}
	}

    #[inline]
	pub fn get_bool(&self, param: &str, default: bool) -> bool{
		match self.params.get(param) {
			Some(f) => {
				let s = f.as_str();
				if default { s != "false" } else { s != "true" }
			}
			None => default
		}
	}

    #[inline]
	pub(crate) fn len(&self) -> usize {
		self.params.len()
	}
}

impl Display for  CfgMod {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.val)
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use super::yaml_rust::yaml::Yaml::Array;

    #[test]
    fn statement_deser_test() {
        let mut schema = FilesMap::new();
        match load_dir(&"./tests", &mut schema, "*", "1.1", "1.1", true, true) {
            Ok(_) => {
                assert_eq!(schema.schemas.len(), 1);
                // assert_eq!(schema.schemas.iter().next().unwrap().1.tables.len(), 1);
                assert_eq!(schema.re_runnable.len(), 1);
                assert_eq!(schema.versions.len(), 1);
                assert_eq!(schema.after.len(), 2);
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
		assert!(STATEMENT_LOG_TBL.columns.len() > 16);
	}
}
