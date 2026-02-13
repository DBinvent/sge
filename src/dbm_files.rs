extern crate yaml_rust;

use std::collections::{BTreeMap, HashMap, BTreeSet, HashSet};
use std::fs;

use semver::{SemVerError, Version};


use serde::{Serialize, Serializer};
use std::fmt::Formatter;
use std::result::Result;

use crate::dbm_cfg::ConfigMap;
use slog::Logger;
use std::cmp::Ordering;
use std::fmt;
use std::fmt::Display;
use std::convert::TryFrom;
use schema_guard::load_schema_from_file;
use schema_guard::schema::Schema;
use schema_guard::utils::{Named, OrderedHashMap};
use yaml_validator::Context;


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
    let mut dir = cfg.get("source_dir");
    if dir.len() == 0 { dir.push_str("./"); }
    else if dir.as_bytes()[dir.len()-1] != b'/' { dir.push('/'); }

    let yschema = schema_guard::get_schema();
    let context = Context::try_from(&yschema[..])
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

#[inline(never)]
fn load_dir(
    root: &str,
    dir: &str,
    schema: &mut FilesMap,
    ff: &FileFilters,
    recursive: bool,
    context: &Context,
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
                                        match load_schema_from_file(fi.as_str()) {
                                            Ok(y) => match y["database"].as_vec() {
                                                None => {}
                                                Some(schemas) => {
                                                    let mut schema_schemas = OrderedHashMap::new();
                                                    for s in schemas {
                                                        match schema_schemas.get_mut(&Schema::schema_name(s)) {
                                                            None => {
                                                                let mut ss = Schema::new(&s, file_name.as_str());
                                                                let _ = ss.append(s)?;
                                                                let _ = schema_schemas.append(ss);
                                                            }
                                                            Some(ss) => ss.append(s)?
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


#[cfg(test)]
mod tests {
    use super::*;
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

}
