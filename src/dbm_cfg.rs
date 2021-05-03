extern crate yaml_rust;

use std::{env, fs};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::fmt::{Formatter, Write};
use std::fmt;
use std::fmt::Display;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::result::Result;

use postgres::Transaction;
use semver::{SemVerError, Version};
use serde::Serialize;
use slog::Logger;
use yaml_rust::YamlLoader;

use crate::dbm_dbc::{FKTable, InfoSchemaType, STable};
use crate::dbm_log::StatementLog;

use self::yaml_rust::Yaml;
use crate::dbm_yml::{str2bool, safe_sql};

#[derive(Debug)]
pub struct ConfigMap {
    map: HashMap <String, String>,
    file: String,
    file_total_lines: usize,
    file_loaded_lines: usize,
}

impl ConfigMap {

    /// universal properties loader
    #[inline]
    pub fn load(filename: &str) -> Result<Self, String> {
        let mut file_total_lines = 0;
        let mut file_loaded_lines = 0;
        let mut cfg = HashMap::new();
        if filename.len() > 0 {
            let reader = BufReader::new(File::open(filename)
                .map_err(|e| format!("Can not load file: {} - {}", filename, e))?);
            for line in reader.lines() {
                file_total_lines += 1;
                if let Ok(l) = line {
                    if let Some(i) = l.chars().position(|c| c == '=' || c == ':' || c == '#') {
                        if l.as_bytes()[i] != b'#' {
                            let key = l[..i].trim().to_lowercase();
                            if crate::CONFIG_APP.contains_key(&key) {
                                ConfigMap::map_insert(&mut cfg, &key, l[i + 1..].trim());
                                file_loaded_lines += 1;
                            } else {
                                println!("ignored {} on {}", key, filename);
                            }
                        }
                    }
                }
            }
        }
        Ok(ConfigMap {map: cfg, file: filename.to_string(), file_total_lines, file_loaded_lines })
    }

    #[inline]
    fn unquote(v: &str) -> String {
        if v.len() > 2
            && v.as_bytes()[0] == v.as_bytes()[v.len()-1]
            && (v.as_bytes()[0] == b'"' || v.as_bytes()[0] == b'\'')
        {
            v[1..v.len()-1].into()
        } else { v.to_string() }
    }

    #[inline]
    pub fn insert(&mut self, key: &String, value: &str) {
        ConfigMap::map_insert(&mut self.map, key, value);
    }

    #[inline]
    fn map_insert(map: &mut HashMap <String, String>, key: &String, value: &str) {
        if key.ends_with("_url") {
            // TODO parse URL
            panic!("unimplemented ")
        }
        let _ = map.insert(key.into(), ConfigMap::unquote(value));
    }

    #[inline]
    pub fn get_def(&self, param: &str, def: String) -> String {
        match &self.get_opt(param) {
            None => def,
            Some(v) => v.into()
        }
    }

    #[inline]
    pub fn get(&self, param: &str) -> String {
        match &self.get_opt(param) {
            None => match crate::CONFIG_APP.get(param){
                None => "".to_string(),
                Some(v) => {
                    match v {
                        None => "".to_string(),
                        Some(r) => r.into()
                    }
                }
            }
            Some(v) => v.into()
        }
    }

    #[inline]
    pub fn get_opt(&self, param: &str) -> Option<String> {
            match self.map.get(param) {
                Some(x) => Some(x.clone()),
                None => match env::var(param.to_uppercase()) {
                    Ok(v) => Some(ConfigMap::unquote(v.as_str())), Err(_) => None,
            }
        }
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
            self.get_def(param, format!("{}", default)).as_str(),
            default,
        )
    }

    /// find the target database name for log storage, use a database from connection if not set use '' as current db
    #[inline]
    pub(crate) fn target(&self) -> String {
        self.get("db_target")
    }

    #[inline]
    pub(crate) fn schema(&self) -> String {
        safe_sql(self.get("schema"))
    }

    #[inline]
    pub(crate) fn history_table_name(&self) -> String {
        safe_sql(self.get_def("table", crate::STATEMENT_LOG_TBL.table_name.clone()))
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

impl Display for ConfigMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _ = write!(f, "Config [{}]", self.map.len());
        let _ = write!(f, " loaded from: {}, {}/{}",  self.file, self.file_loaded_lines, self.file_total_lines);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn _conf(p: &str) -> ConfigMap{
        let mut map = HashMap::new();
        map.insert("port".to_string(), p.to_string());
        ConfigMap { map, file: "".to_string(), file_total_lines: 0, file_loaded_lines: 0 }
    }

    #[test]
    fn cfg_test() -> Result<(), String>{
        let a = ConfigMap::load("tests/test.cfg")?;
        let b = ConfigMap::load("tests/test_cfg.yaml")?;
        assert_eq!(a.get_def("db_host", "a".into()), b.get_def("db_host", "b".into()));
        assert_eq!(a.get_def("db_port", "a".into()), b.get_def("db_port", "b".into()));
        println!("{}", a);
        Ok(())
    }

}