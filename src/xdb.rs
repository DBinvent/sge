//! [xdb] sge for another database than PostgreSQL: the core of what sge does,
//! for the binaries sge-sqlite and sge-mysql - a directory applied in sge's file order, each file
//! once, kept in the same history table (`sge_history`); a plan (a dry run); a snapshot of the
//! database written as a YAML schema with its `source`.
//!
//! - `S<version>__<name>.yaml` (or `.yml`): a YAML schema, deployed by schema_guard_tokio through
//!   the database's driver. Another vendor's schema - its `source`, none being PostgreSQL's - goes
//!   through the built-in mapping first, for this target only; its custom SQL is left out there.
//! - `V<version>__<name>.sql`: a script, run once by the database's own client tool (`sqlite3`,
//!   `mysql`), as sge runs psql. Changed after it ran, it is refused, as Flyway does.
//! - `R__<name>.sql`: a repeatable script, run again whenever it changed.
//!
//! Versions as sge reads them (`1_2` is 1.2.0), schemas before scripts of the same version, the
//! repeatable ones last; a file named otherwise is passed over. A schema runs again when it
//! changed. A file that fails stops the run; its failure is kept in the history too.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use md5::{Digest, Md5};
use schema_guard_tokio::column::{Column, Constr, ForeignKey, Index};
use schema_guard_tokio::loader::{InfoSchemaType, PgColumnDfn};
use schema_guard_tokio::mapping::{Mapping, Source};
use schema_guard_tokio::schema::Schema;
use schema_guard_tokio::table::Table;
use schema_guard_tokio::utils::OrderedHashMap;
use schema_guard_tokio::MigrationOptions;
use semver::Version;
use serde::Serialize;
use yaml_rust2::{Yaml, YamlLoader};

/// the history table, as sge keeps it on PostgreSQL (hl_table.yaml), mapped to the target
const HISTORY_YML: &str = include_str!("hl_table.yaml");
pub const HISTORY: &str = "sge_history";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// a YAML schema
    S,
    /// a versioned script
    V,
    /// a repeatable script
    R,
}

impl Kind {
    fn letter(&self) -> &'static str {
        match self { Kind::S => "S", Kind::V => "V", Kind::R => "R" }
    }
}

/// a file of the directory, as sge orders it
#[derive(Debug, Clone)]
pub struct Script {
    pub kind: Kind,
    pub version: Version,
    /// the version as the file names it: `1.2`
    pub version_text: String,
    /// its name in the directory, the history's `source`
    pub name: String,
    pub path: PathBuf,
    pub checksum: String,
}

/// What a run does
#[derive(Debug, Clone, Default)]
pub struct Opts {
    pub dir: PathBuf,
    /// the plan only: what would run, nothing run
    pub plan: bool,
    /// up to this version (repeatable scripts at any)
    pub target: Option<Version>,
    pub verbose: bool,
}

/// a file's MD5, as sge writes it to the history: base64
pub fn checksum(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(Md5::digest(bytes))
}

fn version_of(text: &str) -> Result<Version, String> {
    crate::dbm_files::semver_parse(text)
}

/// The files of a directory sge applies, in its order
pub fn scripts(dir: &Path, target: Option<&Version>) -> Result<Vec<Script>, String> {
    let mut out = Vec::new();
    let mut names: Vec<_> = std::fs::read_dir(dir).map_err(|e| format!("{}: {}", dir.display(), e))?
        .filter_map(|e| e.ok()).filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().to_string()).collect();
    names.sort();
    for name in names {
        let low = name.to_lowercase();
        let (kind, rest) = if (low.ends_with(".yaml") || low.ends_with(".yml")) && name.starts_with('S') {
            (Kind::S, &name[1..])
        } else if low.ends_with(".sql") && name.starts_with('V') {
            (Kind::V, &name[1..])
        } else if low.ends_with(".sql") && name.starts_with("R__") {
            (Kind::R, &name[1..])
        } else {
            continue;
        };
        let Some(i) = rest.find("__") else { continue };
        let version_text = rest[..i].replace('_', ".");
        let version = version_of(&version_text).map_err(|e| format!("{}: {}", name, e))?;
        if kind != Kind::R {
            if let Some(t) = target {
                if &version > t {
                    continue;
                }
            }
        }
        let path = dir.join(&name);
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {}", path.display(), e))?;
        out.push(Script { kind, version, version_text, name, path, checksum: checksum(&bytes) });
    }
    out.sort_by(|a, b| {
        let ra = a.kind == Kind::R;
        let rb = b.kind == Kind::R;
        ra.cmp(&rb).then(a.version.cmp(&b.version)).then(a.kind.cmp(&b.kind)).then(a.name.cmp(&b.name))
    });
    Ok(out)
}

/// what the history says of a file: its last run
#[derive(Debug, Clone)]
pub struct Ran {
    pub checksum: String,
    pub ok: bool,
}

/// What to do with a file, by the history
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Run,
    Skip,
    /// a versioned script changed after it ran: refused
    Changed,
}

pub fn decide(s: &Script, ran: Option<&Ran>) -> Decision {
    match (s.kind, ran) {
        (_, None) => Decision::Run,
        (_, Some(r)) if !r.ok => Decision::Run,
        (_, Some(r)) if r.checksum == s.checksum => Decision::Skip,
        (Kind::V, Some(_)) => Decision::Changed,
        (_, Some(_)) => Decision::Run,
    }
}

/// a YAML schema as the target takes it: mapped when it is another vendor's
pub fn for_target(y: &Yaml, target: &Source, notes: &mut Vec<String>) -> Result<Yaml, String> {
    let from = Source::of(y)?;
    if from.vendor == target.vendor {
        return Ok(y.clone());
    }
    let m = Mapping::builtin(&from.vendor, &target.vendor)
        .ok_or_else(|| format!("no mapping from {} to {}", from.vendor, target.vendor))??;
    let out = m.apply(y, target).map_err(|r| format!("refused for {}:\n  {}", target.name(), r.join("\n  ")))?;
    notes.extend(out.notes);
    Ok(out.schema)
}

/// sge's history table as a schema of the target's: hl_table.yaml in a schema `public`, mapped
pub fn history_schema(target: &Source) -> Result<Yaml, String> {
    let indented: String = HISTORY_YML.lines().map(|l| format!("          {}\n", l)).collect();
    let text = format!("database:\n  - schemaName: public\n    tables:\n      - table:\n{}", indented);
    let y = YamlLoader::load_from_str(&text).map_err(|e| e.to_string())?.remove(0);
    for_target(&y, target, &mut Vec::new())
}

/// the snapshot sge writes: the schema's own YAML with its source
#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub source: String,
    pub database: Vec<Schema>,
}

/// The catalog as YAML schemas: each table but sge's history, its columns in their order, a
/// column's single-column index on it
pub fn snapshot(info: InfoSchemaType, source: &Source) -> Result<String, String> {
    let mut database = Vec::new();
    let mut dbs: Vec<_> = info.into_iter().collect();
    dbs.sort_by(|a, b| a.0.cmp(&b.0));
    for (schema_name, tables) in dbs {
        let mut tables: Vec<_> = tables.into_values().filter(|t| t.table_name != HISTORY).collect();
        tables.sort_by(|a, b| a.table_name.cmp(&b.table_name));
        let mut ytables = OrderedHashMap::new();
        for t in tables {
            let mut cols: Vec<PgColumnDfn> = t.columns.values().cloned().collect();
            cols.sort_by_key(|c| c.sort_order);
            let mut columns = OrderedHashMap::new();
            for c in cols {
                let index = t.indexes.values().find(|x| x.columns.len() == 1 && x.columns[0].column_name == c.column_name)
                    .map(|x| Index { name: x.index_name.clone(), unique: x.is_unique.then_some(true), concurrently: None,
                                     using: String::new(), order: String::new(), nulls: String::new(), collate: String::new(),
                                     sql: String::new() });
                let pk = t.primary_key.as_ref().map(|k| k.columns.contains(&c.column_name)).unwrap_or(c.pk);
                let _ = columns.append(Column {
                    name: c.column_name.clone(), column_type: c.column_type.clone(), default_value: c.column_default.clone(),
                    constraint: (!c.nullable || pk || c.fk.is_some()).then(|| Constr {
                        primary_key: pk.then_some(true), nullable: c.nullable && !pk,
                        foreign_key: c.fk.clone().map(|f| ForeignKey { references: f.0, sql: String::new() }),
                    }),
                    description: String::new(), sql: String::new(), index, partition_by: None,
                });
            }
            let _ = ytables.append(Table {
                table_name: t.table_name.clone(), description: String::new(), transaction: String::new(), sql: String::new(),
                constraint: String::new(), columns, triggers: OrderedHashMap::new(), data_file: None, data: vec![],
                owner: String::new(), grant: vec![], is_template: None, use_templates: vec![], security: None,
            });
        }
        database.push(Schema { schema_name, owner: String::new(), tables: ytables, file: String::new() });
    }
    serde_saphyr::to_string(&Snapshot { source: source.name(), database }).map_err(|e| e.to_string())
}

/// A database sge-<vendor> works on
#[allow(async_fn_in_trait)]
pub trait Target {
    /// its vendor and version, as a snapshot's source says
    async fn source(&mut self) -> Result<Source, String>;
    /// a YAML schema (already the target's) deployed, or planned only: its statements
    async fn deploy(&mut self, y: Yaml, file: &str, plan: bool) -> Result<Vec<String>, String>;
    /// a script run by the database's client tool
    async fn run_script(&mut self, path: &Path) -> Result<(), String>;
    /// a statement of sge's own (the history), through the driver
    async fn exec(&mut self, sql: &str) -> Result<(), String>;
    /// the history's rows: (source, checksum, ok), oldest first
    async fn history(&mut self) -> Result<Vec<(String, String, bool)>, String>;
    /// the catalog
    async fn info(&mut self) -> Result<InfoSchemaType, String>;
    /// [review 2026-11-19] a string literal in its SQL: SQLite's doubles a quote; MySQL's a backslash too
    fn lit(&self, v: &str) -> String {
        q(v)
    }
}

fn q(v: &str) -> String {
    format!("'{}'", v.replace('\'', "''"))
}

/// what was done to each file: (file, what)
pub type Report = Vec<(String, String)>;

/// The directory applied: the history table made or brought up to date, then each file in order
pub async fn run<T: Target>(t: &mut T, opts: &Opts) -> Result<Report, String> {
    let source = t.source().await?;
    let files = scripts(&opts.dir, opts.target.as_ref())?;
    let mut report = Vec::new();
    let hist = t.deploy(history_schema(&source)?, "hl_table.yaml", opts.plan).await?;
    if opts.plan && !hist.is_empty() {
        report.push((HISTORY.to_string(), format!("made: {} statement(s)", hist.len())));
    }
    let mut last: HashMap<String, Ran> = HashMap::new();
    if !(opts.plan && !hist.is_empty()) {
        for (src, sum, ok) in t.history().await? {
            last.insert(src, Ran { checksum: sum, ok });
        }
    }
    for s in &files {
        match decide(s, last.get(&s.name)) {
            Decision::Skip => {
                if opts.verbose {
                    report.push((s.name.clone(), "unchanged".into()));
                }
                continue;
            }
            Decision::Changed => return Err(format!("{}: changed after it ran (checksum {} then, {} now) - a versioned \
                                                     script runs once; write a new version", s.name,
                                                    last[&s.name].checksum, s.checksum)),
            Decision::Run => {}
        }
        let started = Instant::now();
        let result: Result<String, String> = match s.kind {
            Kind::S => {
                // [review 2026-11-19] a file that does not read or parse is a failure kept in the history too
                let read = std::fs::read_to_string(&s.path).map_err(|e| e.to_string())
                    .and_then(schema_guard_tokio::load_schema_from_src);
                let mut notes = Vec::new();
                match read.and_then(|y| for_target(&y, &source, &mut notes)) {
                    Err(e) => Err(e),
                    Ok(y) => t.deploy(y, &s.name, opts.plan).await.map(|st| {
                        let mut w = format!("{} statement(s)", st.len());
                        if opts.plan || opts.verbose {
                            for x in &st { w.push_str(&format!("\n    {}", x)); }
                        }
                        for n in &notes { w.push_str(&format!("\n    note: {}", n)); }
                        w
                    }),
                }
            }
            Kind::V | Kind::R => if opts.plan { Ok("would run".into()) } else { t.run_script(&s.path).await.map(|_| "ran".into()) },
        };
        if opts.plan {
            match result {
                Ok(w) => report.push((s.name.clone(), w)),
                Err(e) => return Err(format!("{}: {}", s.name, e)),
            }
            continue;
        }
        let (ok, msg) = match &result { Ok(_) => (true, String::new()), Err(e) => (false, e.chars().take(250).collect::<String>()) };
        let insert = format!("INSERT INTO {} (source, source_type, version, checksum, ok, msg, took_sec, size) \
                              VALUES ({}, {}, {}, {}, {}, {}, {}, {})", HISTORY, t.lit(&s.name), t.lit(s.kind.letter()),
                             t.lit(&s.version_text), t.lit(&s.checksum), if ok { "1" } else { "0" }, t.lit(&msg),
                             started.elapsed().as_secs(), std::fs::metadata(&s.path).map(|m| m.len()).unwrap_or(0));
        // [review 2026-11-19] the history not written: said with what the file did, so neither is lost
        if let Err(e) = t.exec(&insert).await {
            return Err(match &result {
                Ok(_) => format!("{}: applied, but its history row was not written - the next run applies it again: {}", s.name, e),
                Err(r) => format!("{}: {}\n  and its failure was not written to the history: {}", s.name, r, e),
            });
        }
        match result {
            Ok(w) => report.push((s.name.clone(), w)),
            Err(e) => return Err(format!("{}: {}", s.name, e)),
        }
    }
    Ok(report)
}

/// sge-sqlite's database: a file (made when not there), databases attached to it as schemas
#[cfg(feature = "sqlite")]
pub mod sqlite {
    use super::*;

    pub struct Sqlite {
        pub path: String,
        /// (schema, file) attached on every connection
        pub attach: Vec<(String, String)>,
        /// the client tool for scripts
        pub client: String,
    }

    impl Sqlite {
        /// a connection of its own each time: the client tool writes the file in between
        async fn conn(&self) -> Result<turso::Connection, String> {
            let db = turso::Builder::new_local(&self.path).experimental_attach(true).build().await
                .map_err(|e| format!("{}: {}", self.path, e))?;
            let c = db.connect().map_err(|e| e.to_string())?;
            for (name, file) in &self.attach {
                c.execute_batch(&format!("ATTACH DATABASE {} AS \"{}\"", q(file), name.replace('"', "\"\""))).await
                    .map_err(|e| format!("attach {}: {}", name, e))?;
            }
            Ok(c)
        }
    }

    impl Target for Sqlite {
        async fn source(&mut self) -> Result<Source, String> {
            Source::parse("sqlite.3")
        }

        async fn deploy(&mut self, y: Yaml, file: &str, plan: bool) -> Result<Vec<String>, String> {
            let c = self.conn().await?;
            schema_guard_tokio::sqlite::migrate(y, &c, plan, file, &MigrationOptions::default()).await.map(|p| p.statements)
        }

        async fn run_script(&mut self, path: &Path) -> Result<(), String> {
            let input = std::fs::File::open(path).map_err(|e| format!("{}: {}", path.display(), e))?;
            let mut args = vec!["-bail".to_string()];
            for (name, file) in &self.attach {
                args.push("-cmd".into());
                args.push(format!("ATTACH DATABASE {} AS \"{}\"", q(file), name.replace('"', "\"\"")));
            }
            args.push(self.path.clone());
            let out = std::process::Command::new(&self.client).args(&args).stdin(input).output()
                .map_err(|e| format!("{}: {} - is it installed and on PATH?", self.client, e))?;
            if out.status.success() {
                Ok(())
            } else {
                Err(format!("{} {}: {}", self.client, out.status, String::from_utf8_lossy(&out.stderr).trim()))
            }
        }

        async fn exec(&mut self, sql: &str) -> Result<(), String> {
            self.conn().await?.execute_batch(sql).await.map_err(|e| format!("{}\n  in: {}", e, sql))
        }

        async fn history(&mut self) -> Result<Vec<(String, String, bool)>, String> {
            let c = self.conn().await?;
            let mut r = c.query(&format!("SELECT source, checksum, ok FROM {} ORDER BY id", HISTORY), ()).await.map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            while let Some(row) = r.next().await.map_err(|e| e.to_string())? {
                let text = |i: usize| match row.get_value(i) { Ok(turso::Value::Text(s)) => s, _ => String::new() };
                let ok = matches!(row.get_value(2), Ok(turso::Value::Integer(n)) if n != 0);
                out.push((text(0), text(1), ok));
            }
            Ok(out)
        }

        async fn info(&mut self) -> Result<InfoSchemaType, String> {
            schema_guard_tokio::sqlite::load_info_schema(&self.conn().await?).await
        }
    }
}

/// sge-mysql's database: a URL, mysql://user:password@host:port/database
#[cfg(feature = "mysql")]
pub mod mysql {
    use super::*;
    use sqlx::{Connection, Row};

    pub struct Mysql {
        pub url: String,
        pub conn: sqlx::MySqlConnection,
        /// the client tool for scripts
        pub client: String,
    }

    /// a URL's parts for the client tool
    #[derive(Debug, Clone, Default, PartialEq)]
    pub struct Url {
        pub host: String,
        pub port: String,
        pub user: String,
        pub password: String,
        pub database: String,
        /// `?socket=`: the client goes by it, as sqlx does
        pub socket: Option<String>,
    }

    /// a percent-escape decoded: `%40` is `@`; a `%` not followed by two hex digits kept as it is
    fn decode(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            // [review 2026-11-19] the digits tested as bytes: a slice of the text panicked inside a multibyte char
            if b[i] == b'%' && i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
                let hex = |c: u8| (c as char).to_digit(16).unwrap() as u8;
                out.push(hex(b[i + 1]) * 16 + hex(b[i + 2]));
                i += 3;
                continue;
            }
            out.push(b[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).to_string()
    }

    /// mysql://user:password@host:port/database?socket=... - the user info up to the last `@`
    /// before the path, so a `?` or `/` in an escaped password is the password's; an IPv6 host
    /// in brackets [[review 2026-11-19]]
    pub fn parts(url: &str) -> Result<Url, String> {
        let rest = url.strip_prefix("mysql://").or_else(|| url.strip_prefix("mariadb://"))
            .ok_or_else(|| "a mysql:// URL".to_string())?;
        let at = rest.rfind('@');
        let (auth, after) = match at { Some(i) => (&rest[..i], &rest[i + 1..]), None => ("", rest) };
        let (hostport, path) = match after.find(|c| c == '/' || c == '?') {
            Some(i) => (&after[..i], &after[i..]),
            None => (after, ""),
        };
        let (dbpart, query) = match path.split_once('?') { Some((d, q)) => (d, q), None => (path, "") };
        let (user, pw) = match auth.split_once(':') { Some((u, p)) => (u, p), None => (auth, "") };
        let (host, port) = if let Some(h) = hostport.strip_prefix('[') {
            match h.split_once(']') {
                Some((h6, rest)) => (h6.to_string(), rest.strip_prefix(':').unwrap_or("3306").to_string()),
                None => return Err("an IPv6 host without its closing ]".into()),
            }
        } else {
            match hostport.rsplit_once(':') { Some((h, p)) => (h.to_string(), p.to_string()), None => (hostport.to_string(), "3306".into()) }
        };
        let socket = query.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == "socket").map(|(_, v)| decode(v));
        Ok(Url { host, port: if port.is_empty() { "3306".into() } else { port }, user: decode(user), password: decode(pw),
                 database: decode(dbpart.trim_start_matches('/')), socket })
    }

    impl Mysql {
        pub async fn connect(url: &str, client: &str) -> Result<Self, String> {
            let conn = sqlx::MySqlConnection::connect(url).await.map_err(|e| format!("{}: {}", url.split('@').last().unwrap_or(""), e))?;
            Ok(Mysql { url: url.to_string(), conn, client: client.to_string() })
        }
    }

    impl Target for Mysql {
        async fn source(&mut self) -> Result<Source, String> {
            let v: String = sqlx::query_scalar("SELECT VERSION()").fetch_one(&mut self.conn).await.map_err(|e| e.to_string())?;
            let mut it = v.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty());
            let (major, minor) = (it.next().unwrap_or("8"), it.next().unwrap_or("0"));
            Source::parse(&format!("mysql.{}.{}", major, minor))
        }

        async fn deploy(&mut self, y: Yaml, file: &str, plan: bool) -> Result<Vec<String>, String> {
            schema_guard_tokio::mysql::migrate(y, &mut self.conn, plan, file, &MigrationOptions::default()).await.map(|p| p.statements)
        }

        async fn run_script(&mut self, path: &Path) -> Result<(), String> {
            let u = parts(&self.url)?;
            let input = std::fs::File::open(path).map_err(|e| format!("{}: {}", path.display(), e))?;
            let mut cmd = std::process::Command::new(&self.client);
            cmd.args(["--no-defaults", "--batch", "-u", &u.user]);
            // [review 2026-11-19] the server the driver reached: by its socket when the URL names one
            match &u.socket {
                Some(sock) => { cmd.args(["--protocol=SOCKET", "--socket", sock]); }
                None => { cmd.args(["--protocol=TCP", "-h", &u.host, "-P", &u.port]); }
            }
            if !u.database.is_empty() {
                cmd.args(["-D", &u.database]);
            }
            // the password by the environment, not on the command line where ps shows it
            cmd.env("MYSQL_PWD", &u.password);
            let out = cmd.stdin(input).output().map_err(|e| format!("{}: {} - is it installed and on PATH?", self.client, e))?;
            if out.status.success() {
                Ok(())
            } else {
                Err(format!("{} {}: {}", self.client, out.status, String::from_utf8_lossy(&out.stderr).trim()))
            }
        }

        async fn exec(&mut self, sql: &str) -> Result<(), String> {
            sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_string())).execute(&mut self.conn).await.map(|_| ())
                .map_err(|e| format!("{}\n  in: {}", e, sql))
        }

        async fn history(&mut self) -> Result<Vec<(String, String, bool)>, String> {
            let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT CAST(source AS CHAR), CAST(checksum AS CHAR), CAST(ok AS SIGNED) FROM {} ORDER BY id", HISTORY)))
                .fetch_all(&mut self.conn).await.map_err(|e| e.to_string())?;
            Ok(rows.iter().map(|r| (
                r.try_get::<Option<String>, _>(0).ok().flatten().unwrap_or_default(),
                r.try_get::<Option<String>, _>(1).ok().flatten().unwrap_or_default(),
                r.try_get::<Option<i64>, _>(2).ok().flatten().unwrap_or(0) != 0)).collect())
        }

        fn lit(&self, v: &str) -> String {
            // [review 2026-11-19] MySQL reads a backslash in a string as an escape
            format!("'{}'", v.replace('\\', "\\\\").replace('\'', "''"))
        }

        async fn info(&mut self) -> Result<InfoSchemaType, String> {
            let db: Option<String> = sqlx::query_scalar("SELECT DATABASE()").fetch_one(&mut self.conn).await.map_err(|e| e.to_string())?;
            let dbs: Vec<&str> = db.as_deref().into_iter().collect();
            schema_guard_tokio::mysql::load_info_schema(&mut self.conn, &dbs).await
        }
    }
}

#[cfg(test)]
#[path = "xdb_test.rs"]
mod tests;
