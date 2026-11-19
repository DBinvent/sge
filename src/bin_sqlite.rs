//! sge-sqlite: sge's core for SQLite, through turso (embedded, as yt-lite uses it); scripts by sqlite3
use sge::xdb::sqlite::Sqlite;
use sge::xdb_cli;

#[tokio::main]
async fn main() {
    let m = xdb_cli::command("sge-sqlite", "SQLite", "sqlite3")
        .arg(clap::Arg::new("attach").long("attach").value_name("SCHEMA=FILE").action(clap::ArgAction::Append)
            .help("a database attached as that schema (repeatable)"))
        .get_matches();
    let mut attach = Vec::new();
    for a in m.get_many::<String>("attach").into_iter().flatten() {
        match a.split_once('=') {
            Some((s, f)) => attach.push((s.to_string(), f.to_string())),
            None => { eprintln!("--attach {}: SCHEMA=FILE", a); std::process::exit(2); }
        }
    }
    let mut t = Sqlite {
        path: m.get_one::<String>("database").cloned().unwrap_or_default(),
        attach,
        client: m.get_one::<String>("client").cloned().unwrap_or_else(|| "sqlite3".into()),
    };
    std::process::exit(xdb_cli::main_of(&mut t, &m).await);
}
