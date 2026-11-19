//! [xdb] sge-sqlite's and sge-mysql's core: the file order, what runs again, the history table on
//! each, a run on SQLite (in a temp file, its scripts through turso - no sqlite3 needed), and a
//! snapshot that deploys back as it was. A live MySQL run: tests/xdb_mysql.rs (SG_MYSQL_URL).
use super::*;

fn dir(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sge-xdb-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    for (f, text) in files {
        std::fs::write(d.join(f), text).unwrap();
    }
    d
}

const SCHEMA: &str = "database:
  - schemaName: public
    tables:
      - table:
          tableName: users
          columns:
            - column:
                name: id
                type: bigserial
                constraint:
                  primaryKey: true
            - column:
                name: email
                type: varchar(80)
                constraint:
                  nullable: false
                index:
                  name: users_email_key
                  unique: true
";

#[test]
fn files_go_in_sges_order_and_a_target_stops_them() {
    let d = dir("order", &[("V2__b.sql", ""), ("R__view.sql", ""), ("S1__a.yaml", SCHEMA), ("V1_1__a.sql", ""),
                            ("S2__b.yaml", SCHEMA), ("V2__a.sql", ""), ("notes.txt", ""), ("V3.sql", "")]);
    let names: Vec<String> = scripts(&d, None).unwrap().into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["S1__a.yaml", "V1_1__a.sql", "S2__b.yaml", "V2__a.sql", "V2__b.sql", "R__view.sql"]);
    let upto = Version::new(1, 5, 0);
    let names: Vec<String> = scripts(&d, Some(&upto)).unwrap().into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["S1__a.yaml", "V1_1__a.sql", "R__view.sql"], "the repeatable at any target");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_versioned_script_runs_once_and_the_others_again_when_changed() {
    let s = |kind| Script { kind, version: Version::new(1, 0, 0), version_text: "1".into(), name: "x".into(),
                            path: PathBuf::new(), checksum: "now".into() };
    let ran = |sum: &str, ok| Ran { checksum: sum.into(), ok };
    assert_eq!(decide(&s(Kind::V), None), Decision::Run);
    assert_eq!(decide(&s(Kind::V), Some(&ran("now", true))), Decision::Skip);
    assert_eq!(decide(&s(Kind::V), Some(&ran("then", true))), Decision::Changed);
    assert_eq!(decide(&s(Kind::V), Some(&ran("then", false))), Decision::Run, "a failed one runs again");
    assert_eq!(decide(&s(Kind::R), Some(&ran("then", true))), Decision::Run);
    assert_eq!(decide(&s(Kind::S), Some(&ran("then", true))), Decision::Run);
    assert_eq!(decide(&s(Kind::S), Some(&ran("now", true))), Decision::Skip);
}

#[cfg(feature = "mysql")]
#[test]
fn the_history_table_is_mysqls_by_the_mapping() {
    let y = history_schema(&Source::parse("mysql.8.0").unwrap()).unwrap();
    let s = schema_guard_tokio::parse_yaml_schema(y, "hl_table.yaml").unwrap();
    let named: Vec<(String, &Schema)> = s.list.iter().map(|x| ("app".to_string(), x)).collect();
    let p = schema_guard_tokio::mysql::plan_on(&named, &Default::default(), &MigrationOptions::default()).unwrap();
    let create = p.statements.iter().find(|x| x.starts_with("CREATE TABLE `app`.`sge_history`")).unwrap();
    assert!(create.contains("`id` int auto_increment NOT NULL"), "{}", create);
    assert!(create.contains("`ok` tinyint(1) NOT NULL DEFAULT false"), "{}", create);
    let u = mysql::parts("mysql://ro%40t:p%3Aw@db.example:3307/app?ssl-mode=required").unwrap();
    assert_eq!((u.host.as_str(), u.port.as_str(), u.user.as_str(), u.password.as_str(), u.database.as_str()),
               ("db.example", "3307", "ro@t", "p:w", "app"));
    // [review 2026-11-19] a `?` and `@` in the password, IPv6, a socket, a stray % before a multibyte char
    let u = mysql::parts("mysql://me:a?b@c%2F@[::1]/app?socket=%2Frun%2Fmysqld.sock").unwrap();
    assert_eq!((u.host.as_str(), u.port.as_str(), u.password.as_str(), u.database.as_str()), ("::1", "3306", "a?b@c/", "app"));
    assert_eq!(u.socket.as_deref(), Some("/run/mysqld.sock"));
    let u = mysql::parts("mysql://me:%1\u{e9}@[::1]:3307").unwrap();
    assert_eq!((u.host.as_str(), u.port.as_str(), u.password.as_str(), u.database.as_str()), ("::1", "3307", "%1\u{e9}", ""));
    assert!(mysql::parts("postgres://x").is_err());
}

/// sge-sqlite's target with its scripts run through turso: the test needs no sqlite3
#[cfg(feature = "sqlite")]
struct TestSqlite(sqlite::Sqlite);

#[cfg(feature = "sqlite")]
impl Target for TestSqlite {
    async fn source(&mut self) -> Result<Source, String> { self.0.source().await }
    async fn deploy(&mut self, y: Yaml, file: &str, plan: bool) -> Result<Vec<String>, String> { self.0.deploy(y, file, plan).await }
    async fn run_script(&mut self, path: &Path) -> Result<(), String> {
        let sql = std::fs::read_to_string(path).unwrap();
        self.0.exec(&sql).await
    }
    async fn exec(&mut self, sql: &str) -> Result<(), String> { self.0.exec(sql).await }
    async fn history(&mut self) -> Result<Vec<(String, String, bool)>, String> { self.0.history().await }
    async fn info(&mut self) -> Result<InfoSchemaType, String> { self.0.info().await }
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_directory_runs_on_sqlite_once_and_a_failure_is_kept_and_stops() {
    let d = dir("run", &[("S1__a.yaml", SCHEMA), ("V1_1__data.sql", "INSERT INTO users (email) VALUES ('a@x.com');"),
                          ("R__view.sql", "DROP VIEW IF EXISTS emails; CREATE VIEW emails AS SELECT email FROM users;")]);
    let file = d.join("app.db").to_string_lossy().to_string();
    let mut t = TestSqlite(sqlite::Sqlite { path: file.clone(), attach: vec![], client: "sqlite3".into() });
    let o = Opts { dir: d.clone(), ..Opts::default() };

    let plan = run(&mut t, &Opts { plan: true, ..o.clone() }).await.unwrap();
    assert_eq!(plan[0].0, HISTORY);
    assert!(t.info().await.unwrap()["main"].is_empty(), "a plan runs nothing");

    let first = run(&mut t, &o).await.unwrap();
    assert_eq!(first.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(), ["S1__a.yaml", "V1_1__data.sql", "R__view.sql"]);
    assert!(run(&mut t, &o).await.unwrap().is_empty(), "nothing the second time");
    let h = t.history().await.unwrap();
    assert_eq!(h.len(), 3);
    assert!(h.iter().all(|r| r.2));

    // a versioned script changed after it ran is refused
    std::fs::write(d.join("V1_1__data.sql"), "INSERT INTO users (email) VALUES ('b@x.com');").unwrap();
    assert!(run(&mut t, &o).await.unwrap_err().contains("changed after it ran"));
    std::fs::write(d.join("V1_1__data.sql"), "INSERT INTO users (email) VALUES ('a@x.com');").unwrap();

    // a failing one: kept as failed, and the run stops at it
    std::fs::write(d.join("V2__bad.sql"), "INSERT INTO nowhere VALUES (1);").unwrap();
    std::fs::write(d.join("V3__after.sql"), "INSERT INTO users (email) VALUES ('c@x.com');").unwrap();
    assert!(run(&mut t, &o).await.unwrap_err().starts_with("V2__bad.sql"));
    let h = t.history().await.unwrap();
    assert!(h.iter().any(|r| r.0 == "V2__bad.sql" && !r.2));
    assert!(!h.iter().any(|r| r.0 == "V3__after.sql"), "the run stopped");
    let _ = std::fs::remove_dir_all(d);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn a_snapshot_deploys_back_as_it_was() {
    let d = dir("snap", &[("S1__a.yaml", SCHEMA)]);
    let file = d.join("app.db").to_string_lossy().to_string();
    let mut t = TestSqlite(sqlite::Sqlite { path: file, attach: vec![], client: "sqlite3".into() });
    run(&mut t, &Opts { dir: d.clone(), ..Opts::default() }).await.unwrap();
    let yaml = snapshot(t.info().await.unwrap(), &Source::parse("sqlite.3").unwrap()).unwrap();
    assert!(yaml.starts_with("source: sqlite.3"), "{}", yaml);
    assert!(!yaml.contains(HISTORY), "sge's own table is left out");
    let y = schema_guard_tokio::load_schema_from_src(yaml.clone()).unwrap_or_else(|e| panic!("{}\n{}", e, yaml));
    let mut notes = Vec::new();
    let same = for_target(&y, &Source::parse("sqlite.3").unwrap(), &mut notes).unwrap();
    assert!(notes.is_empty(), "the same vendor: nothing mapped");
    assert!(t.deploy(same, "S1__snap.yaml", false).await.unwrap().is_empty(), "the snapshot is what is there");
    let _ = std::fs::remove_dir_all(d);
}
