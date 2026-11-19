//! [xdb] The command line sge-sqlite and sge-mysql share: the database, the directory, and what
//! to do. sge's own options are PostgreSQL's (i18n/*_clc.yaml); these are the core's alone.
use clap::{Arg, ArgAction, ArgMatches, Command};
use std::path::PathBuf;

pub fn command(name: &'static str, database: &'static str, client: &'static str) -> Command {
    Command::new(name)
        .version(env!("CARGO_PKG_VERSION"))
        .about(format!("Schema Guard Enterprise for {}: a directory of YAML schemas (S<version>__name.yaml) and SQL \
                        scripts (V<version>__name.sql, R__name.sql) applied in order, each once, kept in sge_history; \
                        or the database's schema written as YAML", database))
        .arg(Arg::new("database").required(true).help(match name {
            "sge-sqlite" => "the SQLite file (made when not there)",
            _ => "the MySQL URL: mysql://user:password@host:3306/database",
        }))
        .arg(Arg::new("dir").default_value(".").help("the directory of schemas and scripts"))
        .arg(Arg::new("plan").long("plan").action(ArgAction::SetTrue).help("say what would run, run nothing"))
        .arg(Arg::new("snapshot").long("snapshot").value_name("FILE")
            .help("write the database's schema as YAML to FILE (- for the output), and nothing else"))
        .arg(Arg::new("target").long("target").value_name("VERSION").help("apply up to this version"))
        .arg(Arg::new("client").long("client").value_name("PATH").default_value(client)
            .help("the client tool that runs the scripts"))
        .arg(Arg::new("verbose").short('v').long("verbose").action(ArgAction::SetTrue).help("say the unchanged files too"))
}

pub fn opts(m: &ArgMatches) -> Result<crate::xdb::Opts, String> {
    Ok(crate::xdb::Opts {
        dir: PathBuf::from(m.get_one::<String>("dir").cloned().unwrap_or_else(|| ".".into())),
        plan: m.get_flag("plan"),
        target: match m.get_one::<String>("target") {
            Some(t) => Some(crate::dbm_files::semver_parse(t)?),
            None => None,
        },
        verbose: m.get_flag("verbose"),
    })
}

/// a run or a snapshot on a target, said on the output; the exit code 1 on a failure
pub async fn main_of<T: crate::xdb::Target>(t: &mut T, m: &ArgMatches) -> i32 {
    let r = async {
        if let Some(file) = m.get_one::<String>("snapshot") {
            let source = t.source().await?;
            let yaml = crate::xdb::snapshot(t.info().await?, &source)?;
            if file == "-" {
                print!("{}", yaml);
            } else {
                std::fs::write(file, yaml).map_err(|e| format!("{}: {}", file, e))?;
                println!("{}: written", file);
            }
            return Ok(());
        }
        let o = opts(m)?;
        for (file, what) in crate::xdb::run(t, &o).await? {
            println!("{}: {}", file, what);
        }
        Ok::<(), String>(())
    }.await;
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{}", e);
            1
        }
    }
}
