extern crate yaml_rust;

use std::collections::HashMap;
use std::fmt;
use std::fmt::Display;
use std::fmt::Write;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::result::Result;
use std::env;

#[allow(unused, warnings)]
use chrono::{Utc, Duration};
use schema_guard::utils::{safe_sql_name, str2bool};
use slog::Logger;
use sloggers::Build;
use sloggers::terminal::{Destination, TerminalLoggerBuilder};
use sloggers::types::{Format, Severity, SourceLocation};
use crate::dbm_log::StatementLog;

#[derive(Debug, Clone)]
pub struct ConfigMap {
    map: HashMap<String, String>,
    file: String,
    file_total_lines: usize,
    file_loaded_lines: usize,
    pub log: Logger
}

#[inline]
pub fn logger(debug: bool) -> Logger {
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

impl ConfigMap {
    /// universal properties loader, or create default id filename is empty
    #[inline]
    pub fn load(filename: &str) -> Result<Self, String> {
        let mut file_total_lines = 0;
        let mut file_loaded_lines = 0;
        let mut cfg = HashMap::new();
        if filename.len() > 0 {
            let reader = BufReader::new(
                File::open(filename)
                    .map_err(|e| format!("Can not load file: {} - {}", filename, e))?,
            );
            for line in reader.lines() {
                file_total_lines += 1;
                if let Ok(l) = line {
                    if let Some(i) = l.chars().position(|c| c == '=' || c == ':' || c == '#' || c == ';' || c == '/' || c == '[') {
                        if l.as_bytes()[i] != b'#' && l.as_bytes()[i] != b';' && l.as_bytes()[i] != b'/' && l.as_bytes()[i] != b'[' {
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
        let log = logger(str2bool(cfg.get("verbose").unwrap_or(&"".to_string()), false));
        Ok(ConfigMap {
            map: cfg,
            file: filename.to_string(),
            file_total_lines,
            file_loaded_lines,
            log
        })
    }

    #[inline]
    pub fn unquote(v: &str) -> String {
        let last = if v.len() > 2 {
            if v.as_bytes()[v.len() - 1] == b'\n' {
                v.len() - 2
            } else {
                v.len() - 1
            }
        } else { 0 };

        if v.len() > 2 && v.as_bytes()[0] == v.as_bytes()[last]
            && (v.as_bytes()[0] == b'"' || v.as_bytes()[0] == b'\'')
        {
            v[1..last].into()
        } else {
            v.to_string()
        }
    }

    #[inline]
    pub fn insert(&mut self, key: &String, value: &str) {
        ConfigMap::map_insert(&mut self.map, key, value);
    }

    #[inline]
    fn map_insert(map: &mut HashMap<String, String>, key: &String, value: &str) {
        if key.ends_with("_url") {
            for (k, v) in parse_url(key, value) {
                if !map.contains_key(&k) {
                    let _ = map.insert(k, v);
                }
            }
        } else {
            let _ = map.insert(key.into(), ConfigMap::unquote(value));
        }
    }

    #[inline]
    pub fn get_def(&self, param: &str, def: String) -> String {
        match &self.get_opt(param) {
            None => def,
            Some(v) => v.into(),
        }
    }

    #[inline]
    pub fn get(&self, param: &str) -> String {
        match &self.get_opt(param) {
            None => match crate::CONFIG_APP.get(param) {
                None => "".to_string(),
                Some(v) => match v {
                    None => "".to_string(),
                    Some(r) => r.into(),
                },
            },
            Some(v) => v.into(),
        }
    }

    #[inline]
    pub fn get_opt(&self, param: &str) -> Option<String> {
        match self.map.get(param) {
            Some(x) => Some(x.clone()),
            None => match env::var(param.to_uppercase()) {
                Ok(v) => Some(ConfigMap::unquote(v.as_str())),
                Err(_) => None,
            },
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
    pub(crate) fn get_pow_scr(&self, content: &mut String, prefix: &str, param: &str, amp: &str) {
        if let Some(_) = self.get_opt(format!("{}{}", prefix, param).as_str()) {
            let _ = write!(
                content,
                "{}{}=***",
                if content.len() > 0 { amp } else { "" },
                param
            );
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
    /// if logdb set, than db_target must default to {host/db_name}
    #[inline]
    pub(crate) fn target(&self) -> String {
        self.get_def("db_target", match self.get_opt("logdb_name") {
            None => "".to_string(),
            Some(_) => format!("{}/{}", self.get("db_host"), self.get("db_name"))
        })
    }

    #[inline]
    pub(crate) fn schema(&self) -> String {
        safe_sql_name(self.get("schema"))
    }

    #[inline]
    pub(crate) fn history_table_name(&self) -> String {
        safe_sql_name(self.get_def("table", crate::STATEMENT_LOG_TBL.table_name.clone()))
    }

    #[inline]
    pub(crate) fn is_pgsrv(&self) -> bool {
        if let Some(v) = self.get_opt("db_sslmode") {
            v.as_str() != "disable"
        } else {
            false
        }
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

/// The define the logDB use as primary OR copy.
/// If primary set, than log table wont create on main target DB and log DB be used instead.
    #[inline]
    pub(crate) fn is_primary(&self) -> bool {
        self.get("logdb").as_str() == "primary"
    }

/// Is logDB set
    #[inline]
    pub(crate) fn is_logdb(&self) -> bool {
        self.get_opt("logdb_name").is_some()
    }

/// Is logDB set
    #[inline]
    pub(crate) fn is_retry(&self) -> bool {
        match self.map.get("schema_transaction") {
            None => false,
            Some(x) => x.as_str() == "retry"
        }
    }

    pub fn debug(&self, log: &Logger) {
        debug!(log, "{}", self);
        for (k,v) in &self.map {
            debug!(log, "{}: {}", k, v)
        }
    }

    pub fn dry_run(&self, input: Vec<String>) -> Result<(), String> {
        StatementLog::dry_store(self.get_opt("dry_run_to"), input.into_iter().collect(), &self.log)
            .map(|_| ())
    }
}

impl Display for ConfigMap {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _ = write!(f, "Config [{}]", self.map.len());
        let _ = write!(
            f,
            " loaded from: {}, {}/{}",
            self.file, self.file_loaded_lines, self.file_total_lines
        );
        Ok(())
    }
}


/// "postgresql://[user[:password]@][host][:port][,...][/dbname][?param1=value1&...]"
#[inline]
fn parse_url(key: &String, url: &str) -> HashMap<String, String> {
    let prefix = if key.starts_with("logdb_") { "logdb_".into() } else { "db_".into() };
    let mut map = HashMap::new();
    if let Some(i) = url.find("://") {
        if i > 5 && url[..i].contains("postgres") {
            let url = &url[i + 3 ..];
            let mut vu = Vec::new();
            let mut open = false;
            let mut i = 0;
            let mut f = 0;
            for c in url.as_bytes() {
                let c = *c;
                if c == b'/' || c == b'[' || c == b']' || c == b':'  || c == b',' || c == b'@' || c == b'?' {
                    if !(open && c != b']') {
                        if c == b'[' {
                            open = true;
                        } else if c == b']' {
                            open = false;
                        } else {
                            if i > f {
                                vu.push(&url[f..i])
                            }
                            f = i;
                            if c == b'?' {
                                break;
                            }
                        }
                    }
                }
                i += 1;
            }
            if f < url.len() {
                vu.push(&url[f..]);
            }
            if vu.len() > 0 {
                let mut hosts = 0;
                if let Some(v) = last(&mut vu, b'?') {
                    let v: Vec<&str> = v.split("&").collect();
                    for val in v {
                        parse_kve(&prefix, val.into(), &mut map);
                    }
                }
                if let Some(v) = last(&mut vu, b'/') {
                    map.insert(format!("{}name", prefix), v);
                }
                if let Some(v) = last(&mut vu, b'@') {
                    map.insert(format!("{}host", prefix), unquote(v));
                    hosts += 1;
                }

                loop {
                    if hosts > 0 {
                        if let Some(v) = last(&mut vu, b':') {
                            map.insert(format!("{}password", prefix), v);
                        }
                        if vu.len() == 1 {
                            map.insert(format!("{}user", prefix), vu.remove(0).into());
                        }
                        break;
                    } else {
                        let mut max = 0;
                        let mut ctu = false;
                        loop {
                            if let Some(v) = last(&mut vu, b':') {
                                map.insert(format!("{}port", prefix), v);
                            }
                            if vu.len() == 1 {
                                map.insert(format!("{}host", prefix), unquote(vu.remove(0).into()));
                            } else {
                                if let Some(v) = last(&mut vu, b',') {
                                    map.insert(format!("{}host", prefix), unquote(v));
                                    hosts += 1;
                                }
                                if let Some(v) = last(&mut vu, b'@') {
                                    map.insert(format!("{}host", prefix), unquote(v));
                                    hosts += 1;
                                    ctu = true;
                                    break;
                                }
                            }
                            if vu.len() == 0 || max > 10 {
                                break;
                            }
                            max += 1;
                        }
                        if ctu {
                            continue;
                        }
                    }
                    break;
                }
            }
            // if vu.len() > 0 { println!("\nppr: {:?}", vu); }
        }
    } else {
        let v: Vec<&str>  = url.split(" ").collect();
        for val in v {
            parse_kve(&prefix, val.into(), &mut map);
        }
    }
    map
}

#[inline]
fn last(vu: &mut Vec<&str>, c: u8) -> Option<String> {
    if vu.len() > 0 && vu[vu.len() - 1].as_bytes()[0] == c {
        let v = vu.remove(vu.len() - 1);
        if v.len() > 1 {
            Some(v[1..].into())
        } else {
            Some("".into())
        }
    } else { None }
}

#[inline]
fn unquote(v: String) -> String {
    if v.len() > 2 && v.as_bytes()[0] == b'['
        && v.as_bytes()[v.len() - 1] == b']' {
        v[1..v.len()-1].into()
    } else { v }
}

#[inline]
fn parse_kve(prefix: &String, val: String, map: &mut HashMap<String, String>) {
    if val.len() > 1 {
        if let Some(i) = val.find("=") {
            if i > 0 {
                let key = val[..i].to_string();
                let key = if key.as_str() == "dbname" {
                    "db_name".to_string()
                } else {
                    format!("{}{}", prefix, key)
                };
                map.insert(key, if i + 1 < val.len() { val[i + 1..].to_string() } else { "".to_string() });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Local;
    use std::time::{UNIX_EPOCH, SystemTimeError, SystemTime};

    fn _conf(p: &str) -> ConfigMap {
        let mut map = HashMap::new();
        map.insert("port".to_string(), p.to_string());
        ConfigMap {
            map,
            file: "".to_string(),
            file_total_lines: 0,
            file_loaded_lines: 0,
            log: logger(true),
        }
    }
        // DateTime::naive_local()

    #[test]
    fn cfg_test() -> Result<(), String> {
        let a = ConfigMap::load("tests/test.cfg")?;
        let b = ConfigMap::load("tests/test_cfg.yaml")?;
        assert_eq!(
            a.get_def("db_host", "a".into()),
            b.get_def("db_host", "b".into())
        );
        assert_eq!(
            a.get_def("db_port", "a".into()),
            b.get_def("db_port", "b".into())
        );
        println!("{}", a);
        Ok(())
    }

    #[test]
    fn cfg_testb() -> Result<(), String> {
        let mut a = ConfigMap::load("tests/test.cfg")?;
        assert!(a.get_bool("encrypt", true));
        assert!(!a.get_bool("encrypt", false));
        a.map.insert("encrypt".to_string(), "1".to_string());
        assert!(a.get_bool("encrypt", true));
        assert!(a.get_bool("encrypt", false));
        a.map.insert("encrypt".to_string(), "false".to_string());
        assert!(!a.get_bool("encrypt", true));
        assert!(!a.get_bool("encrypt", false));
        Ok(())
    }

    #[test]
    fn url_test_all()  {

        for url in URLS.iter() {
            let map = parse_url(&"db_url".to_string(), url);
            println!("url: {}\n{:?}\n", url, map);
        }
    }

    const URLS: [&str; 14] = [ "postgresql://",
    "postgresql://localhost",
        "postgresql://localhost:5433",
        "postgresql://localhost/mydb",
        "postgresql://user@localhost",
        "postgresql://user:secret@localhost",
        "postgresql://user:secret@localhost1,localhost2",
        "postgresql://other@localhost/otherdb?connect_timeout=10&application_name=myapp",
        "postgresql://other@[lo:ca:lh:os:t]/otherdb?connect_timeout=10&application_name=myapp",
        "postgresql://host1:123,host2:456/somedb?target_session_attrs=any&application_name=myapp",
        "postgresql://[h:o:s:t:1]:123,[h:o:s:t:2]:456/somedb?target_session_attrs=any&application_name=myapp",
        "postgresql://user:secret@host1:123,host2:456/somedb?target_session_attrs=any&application_name=myapp",
        "url:postgresql:///ytpgxt?user=denis&port=28812&host=/var/run/postgresql",
        "host=<endpoint> user=<userid> dbname=<databasename> port=<port> sslmode=verify-ca sslrootcert=<certificate>"
        ];

    #[test]
    fn url_test_1()  {

        let map = parse_url(&"db_url".to_string(), &URLS[10]);
        println!("\n{}\n{:?}", &URLS[10], map);

    }


    #[test]
    fn date_test() -> Result<(), SystemTimeError>  {
        let now = SystemTime::now();
        let local = Local::now();
        let utc = Utc::now();
        let z = utc.timestamp() - local.timestamp();
        println!("{}", z ); // 3600
        let z = utc.timestamp() - now.duration_since(UNIX_EPOCH)?.as_secs() as i64;
        println!("{}", z ); // 3600
        Ok(())
    }

}
