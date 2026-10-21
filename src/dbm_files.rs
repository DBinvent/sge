
use std::collections::{BTreeMap, HashMap, BTreeSet, HashSet};
use std::fs;

use semver::{Error as SemVerError, Version};


use serde::{Serialize, Serializer};
use std::fmt::Formatter;
use std::result::Result;

use crate::dbm_cfg::ConfigMap;
use slog::Logger;
use std::cmp::Ordering;
use std::fmt;
use std::fmt::Display;
use schema_guard_tokio::{load_schema_from_file, parse_yaml_schema};
use schema_guard_tokio::schema::Schema;
use schema_guard_tokio::utils::{Named, OrderedHashMap};


/// version is a [parse_type]
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FilesMap {
    /// version: filename, schemaName : schema (with name)
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

pub(crate) type SchemaTableMap = BTreeMap<FileVersion, (String, OrderedHashMap<Schema>)>;
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

    // [git] A git source is resolved to a directory and then handled as one.
    // Everything below - filters, version parsing, checksums, history - is
    // deliberately unaware of where the directory came from, so sourcing from
    // git cannot change which scripts run or how they are recorded.
    let mut dir = match crate::dbm_git::materialize(cfg, log)? {
        Some(g) => {
            crate::dbm_git::remember(&g);
            g.path
        }
        None => cfg.get("source_dir"),
    };
    if dir.len() == 0 { dir.push_str("./"); }
    else if dir.as_bytes()[dir.len()-1] != b'/' { dir.push('/'); }

    load_dir(
        dir.as_str(), dir.as_str(),
        &mut schema,
        &FileFilters::new(cfg)?,
        cfg.get_bool("source_recur", true),
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
    pub(crate) fn _pks(&self, schema: &String, table: &String, target: &FileVersion) -> HashSet<String> {
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

/// [sg-parse] An S file, loaded, validated and parsed by schema_guard as its own
/// migrate() parses one: templates resolved, so a `template: [..]` table carries its
/// templates' columns, triggers and grants, and a `template: true` one is marked to be
/// skipped at deploy. None for a file without a `database` list, which sge ignores.
pub(crate) fn parse_schema_file(path: &str, file_name: &str) -> Result<Option<OrderedHashMap<Schema>>, String> {
    let y = load_schema_from_file(path)?;
    if y["database"].as_vec().is_none() {
        return Ok(None);
    }
    parse_yaml_schema(y, file_name).map(Some)
}

#[inline(never)]
fn load_dir(
    root: &str,
    dir: &str,
    schema: &mut FilesMap,
    ff: &FileFilters,
    recursive: bool,
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
                            load_dir(root, fi.as_str(), schema, ff, recursive, log)?;
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
                                        if let Some(schema_schemas) = parse_schema_file(fi.as_str(), file_name.as_str())? {
                                            schema.schemas.insert(
                                                FileVersion::new(vv, key),
                                                (file_name, schema_schemas),
                                            );
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
    // [flyway] no version - Flyway's repeatable `R__name.sql` - is version 0, applied at
    // every target; dbm_log reads a history version it cannot parse as 0 the same way
    if input.is_empty() {
        return Ok(Version::new(0, 0, 0));
    }
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


#[cfg(test)]
mod tests {
    use super::*;

    /// [flyway] Flyway's own file names: `V1_2__init.sql` is version 1.2, and the
    /// unversioned repeatable `R__view.sql` version 0 - which a target never filters out
    #[test]
    fn flyway_file_names_parse() {
        let v = |f: &FileFilter, name: &str, pos: usize| f.parse_type(&name.to_string(), pos).unwrap().map(|(v, _)| v);
        let any = FileFilter { filter: None, file_type: ".sql".into() };
        assert_eq!(v(&any, "V1_2__init.sql", 0), Some(Version::new(1, 2, 0)));
        assert_eq!(v(&any, "V3__more.sql", 0), Some(Version::new(3, 0, 0)));
        assert_eq!(v(&any, "R__view.sql", 0), Some(Version::new(0, 0, 0)));
        assert_eq!(v(&any, "sql/R__view.sql", 4), Some(Version::new(0, 0, 0)), "in a directory");
        let upto = FileFilter { filter: Some(Version::new(1, 0, 0)), file_type: ".sql".into() };
        assert_eq!(v(&upto, "V1_2__init.sql", 0), None, "past the target");
        assert_eq!(v(&upto, "R__view.sql", 0), Some(Version::new(0, 0, 0)));
        assert_eq!(semver_parse("").unwrap(), Version::new(0, 0, 0));
        assert!(semver_parse("x.y").is_err(), "a version that is not one is still an error");
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
            assert_eq!(f.v.patch, i);
            i += 1;
        }
    }

    /// [sg-parse] an S file parses as schema_guard parses it: the template is marked to be
    /// skipped, and a table using it carries the template's columns first, then its own
    #[test]
    fn a_schema_file_resolves_templates_as_schema_guard_does() {
        let dir = std::env::temp_dir().join(format!("sge-templates-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("S1__schema.yaml");
        fs::write(&path, "database:
  - schemaName: tmpl
    tables:
      - table:
          tableName: base_table
          template: true
          columns:
            - column: { name: id, type: serial, constraint: { primaryKey: true } }
            - column: { name: created_at, type: timestamp }
      - table:
          tableName: users
          template: [base_table]
          columns:
            - column: { name: email, type: varchar(255) }
").unwrap();
        let parsed = parse_schema_file(path.to_str().unwrap(), "S1__schema.yaml");
        let _ = fs::remove_dir_all(&dir);
        let schemas = parsed.unwrap().unwrap();
        let tmpl = schemas.get(&"tmpl".to_string()).unwrap();
        assert!(tmpl.tables.get(&"base_table".to_string()).unwrap().is_template());
        let users = tmpl.tables.get(&"users".to_string()).unwrap();
        assert!(!users.is_template());
        let columns: Vec<String> = users.columns.list.iter().map(|c| c.get_name()).collect();
        assert_eq!(columns, vec!["id", "created_at", "email"]);
    }

}
