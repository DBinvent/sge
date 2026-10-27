
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
use schema_guard_tokio::utils::{safe_sql_name, str2bool};
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

/// [url] A value for a connection URL: everything but RFC 3986's unreserved
/// characters percent-encoded - a password with & = % or # broke the URL before
pub(crate) fn url_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for b in v.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b as char),
            _ => { let _ = write!(out, "%{:02X}", b); }
        }
    }
    out
}

#[cfg(test)]
impl ConfigMap {
    /// A map holding exactly these settings, for tests elsewhere in the crate.
    pub(crate) fn of(pairs: &[(&str, &str)]) -> ConfigMap {
        ConfigMap {
            map: pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            file: String::new(),
            file_total_lines: 0,
            file_loaded_lines: 0,
            log: logger(false),
        }
    }
}

#[inline]
pub fn logger(debug: bool) -> Logger {
    logger_to(debug, false)
}

/// The logger, writing to standard error when `stderr` - where standard output carries
/// what was asked for: [superset] a ZIP, which a log line would corrupt
pub fn logger_to(debug: bool, stderr: bool) -> Logger {
    let mut builder = TerminalLoggerBuilder::new();
    builder.channel_size(1024);
    builder.destination(if stderr { Destination::Stderr } else { Destination::Stdout });
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
    /// [superset] The log to standard error, at the level it had: standard output carries
    /// the snapshot's ZIP
    pub fn log_to_stderr(&mut self) {
        self.log = logger_to(self.get_bool("verbose", false), true);
    }

    /// universal properties loader, or create default id filename is empty
    #[inline]
    pub fn load(filename: &str) -> Result<Self, String> {
        let mut file_total_lines = 0;
        let mut file_loaded_lines = 0;
        let mut cfg = HashMap::new();
        if filename.len() > 0 {
            let reader = BufReader::new(
                File::open(filename)
                    .map_err(|e| tr!("cfg.load_file", file = filename, err = e))?,
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
                                println!("{}", tr!("cfg.ignored", key = key, file = filename));
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
                url_value(&v)
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
        f.write_str(&tr!("cfg.display", count = self.map.len(), file = self.file,
            loaded = self.file_loaded_lines, total = self.file_total_lines))
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
                // libpq's `dbname` is the prefix's own `name`: logdb_url's names the log
                // database, never the one migrated
                let key = if key.as_str() == "dbname" {
                    format!("{}name", prefix)
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

    #[test]
    fn url_values_are_percent_encoded() {
        assert_eq!(url_value("plain-value_1.~"), "plain-value_1.~");
        assert_eq!(url_value("a&b=c%d#e f/g"), "a%26b%3Dc%25d%23e%20f%2Fg");
        assert_eq!(url_value("\u{e9}"), "%C3%A9");
    }
    use std::time::{UNIX_EPOCH, SystemTimeError, SystemTime};

    /// [cfg] a properties file: the keys sge knows loaded, quotes taken off; comments,
    /// sections, unknown keys and plain text passed over - and the lines counted
    #[test]
    fn a_config_file_loads_the_keys_sge_knows() {
        let f = std::env::temp_dir().join(format!("sge-cfg-test-{}.cfg", std::process::id()));
        std::fs::write(&f, "db_host = \"db.example\"\ndb_port: 5433\n# db_user=nobody\n; a comment\n\
            // another\n[section]\nno_such_key=1\njust text\nschema='app'\n").unwrap();
        let c = ConfigMap::load(f.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_file(&f);
        assert_eq!(c.get("db_host"), "db.example");
        assert_eq!(c.get("db_port"), "5433");
        assert_eq!(c.get("schema"), "app");
        assert!(!c.map.contains_key("db_user") && !c.map.contains_key("no_such_key"));
        assert_eq!((c.file_loaded_lines, c.file_total_lines), (3, 9));
        assert!(c.to_string().ends_with("3/9"), "{}", c);
        c.debug(&c.log);
        assert!(ConfigMap::load("/nonexistent/sge.cfg").unwrap_err().starts_with("SGE-E101 "));
    }

    #[test]
    fn a_quoted_value_is_unquoted() {
        assert_eq!(ConfigMap::unquote("\"a b\""), "a b");
        assert_eq!(ConfigMap::unquote("'x'\n"), "x", "a line's own newline");
        assert_eq!(ConfigMap::unquote("'x\""), "'x\"", "quotes that do not match");
        assert_eq!(ConfigMap::unquote("''"), "''");
    }

    /// [url] a connection URL fills the keys it names - only those not set already -
    /// under its own prefix
    #[test]
    fn a_connection_url_fills_the_keys_it_names() {
        let get = |c: &ConfigMap, k: &str| c.map.get(k).cloned().unwrap_or_default();
        let mut c = ConfigMap::of(&[("db_port", "6000")]);
        c.insert(&"db_url".to_string(), "postgresql://app:secret@db.example:5433/orders?sslmode=require");
        assert_eq!(get(&c, "db_user"), "app");
        assert_eq!(get(&c, "db_password"), "secret");
        assert_eq!(get(&c, "db_host"), "db.example");
        assert_eq!(get(&c, "db_name"), "orders");
        assert_eq!(get(&c, "db_sslmode"), "require");
        assert_eq!(get(&c, "db_port"), "6000", "set already: kept");

        let mut c = ConfigMap::of(&[]);
        c.insert(&"logdb_url".to_string(), "postgresql://[::1]:5434/logs");
        assert_eq!((get(&c, "logdb_host"), get(&c, "logdb_port"), get(&c, "logdb_name")),
            ("::1".into(), "5434".into(), "logs".into()));
        assert!(!c.map.contains_key("db_name"));

        // libpq's keyword form; `dbname` is the prefix's own name
        let mut c = ConfigMap::of(&[]);
        c.insert(&"logdb_url".to_string(), "host=h user=u dbname=logs port=7 =x");
        assert_eq!((get(&c, "logdb_host"), get(&c, "logdb_user"), get(&c, "logdb_port")),
            ("h".into(), "u".into(), "7".into()));
        assert_eq!(get(&c, "logdb_name"), "logs");
        assert!(!c.map.contains_key("db_name"), "the migrated database is not the log's");

        let mut c = ConfigMap::of(&[]);
        c.insert(&"db_url".to_string(), "postgresql://app@/orders");
        assert_eq!((get(&c, "db_user"), get(&c, "db_host"), get(&c, "db_name")),
            ("app".into(), "".into(), "orders".into()));
    }

    /// [env] a key not configured is read from the environment, upper-cased
    #[test]
    fn a_key_falls_back_to_the_environment() {
        std::env::set_var("SGE_TEST_ONLY_KEY", "'from env'");
        assert_eq!(ConfigMap::of(&[]).get_opt("sge_test_only_key"), Some("from env".to_string()));
        assert_eq!(ConfigMap::of(&[("sge_test_only_key", "set")]).get("sge_test_only_key"), "set");
    }

    /// [tls] TLS is on with a client certificate, unless sslmode says otherwise
    #[test]
    fn tls_is_on_with_a_client_certificate() {
        assert!(!ConfigMap::of(&[]).is_ssl());
        assert!(ConfigMap::of(&[("db_sslcert", "/c.pem")]).is_ssl());
        assert!(ConfigMap::of(&[("db_sslcert", "/c.pem"), ("db_sslmode", "require")]).is_ssl());
        assert!(!ConfigMap::of(&[("db_sslcert", "/c.pem"), ("db_sslmode", "disable")]).is_ssl());
        assert!(!ConfigMap::of(&[("db_sslmode", "require")]).is_ssl());
    }

    #[test]
    fn a_setting_is_written_as_sql() {
        let c = ConfigMap::of(&[("db_host", "h")]);
        let mut s = String::new();
        c.write(&mut s, "db_", "host", "AND");
        c.write(&mut s, "db_", "port", "AND");
        assert_eq!(s, " AND host = 'h'");
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
        "url:postgresql:///app?user=app&port=5432&host=/var/run/postgresql",
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
