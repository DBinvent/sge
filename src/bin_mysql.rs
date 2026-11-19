//! sge-mysql: sge's core for MySQL, through sqlx (pure Rust); scripts by the mysql client
use sge::xdb::mysql::Mysql;
use sge::xdb_cli;

#[tokio::main]
async fn main() {
    let m = xdb_cli::command("sge-mysql", "MySQL", "mysql").get_matches();
    let url = m.get_one::<String>("database").cloned().unwrap_or_default();
    let client = m.get_one::<String>("client").cloned().unwrap_or_else(|| "mysql".into());
    let mut t = match Mysql::connect(&url, &client).await {
        Ok(t) => t,
        Err(e) => { eprintln!("{}", e); std::process::exit(1); }
    };
    std::process::exit(xdb_cli::main_of(&mut t, &m).await);
}
