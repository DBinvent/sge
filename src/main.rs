#![forbid(unsafe_code)]
extern crate slog;

use clap::builder::PossibleValuesParser;
use clap::parser::ValueSource;
use clap::{Arg, ArgAction, ArgMatches, Command};
use yaml_rust2::{Yaml, YamlLoader};

use sge::dbm_cfg::ConfigMap;

// [sg-tokio] sge is now async end to end, because schema_guard_tokio's loader
// and deploy are. A multi-thread runtime rather than current_thread: the
// connection driver task must make progress while a query awaits on it, which
// on a single-threaded runtime it still would - but a future node-by-node roll
// wants real concurrency, and this costs nothing today.
#[tokio::main]
async fn main() {
    // [banner] DBinvent's, on a console run only - stderr is a terminal - so piped
    // output, logs and `sge -V` (build.sh reads it) stay as they were
    if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        eprint!("{}", banner());
    }
    // the version is the crate's, not the yaml's "local_build" placeholder, so every
    // build reports its own (help2man reads it into the man page too)
    let cli = match command(sge::i18n::cl_config()) {
        Ok(c) => c,
        Err(e) => { eprintln!("{}", e); std::process::exit(2); }
    };
    let app = cli.clone().get_matches();
    let cmd = match app.subcommand_name() {
        Some(cmd) => cmd.to_owned(),
        None => {
            println!("{}", cli.clone().render_usage());
            std::process::exit(0);
        }
    };
    let mut cfg = match ConfigMap::load(given(&cli, &app, "config_file").unwrap_or_default().as_str()) {
        Ok(c) => c,
        Err(e) => { eprintln!("{}", e); std::process::exit(2); }
    };
    for ks in sge::CONFIG_APP.keys() {
        if let Some(v) = given(&cli, &app, ks) {
            cfg.insert(ks, v.as_str());
        }
    }
    std::process::exit(match act(cmd.as_str(), cfg).await {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("{}", e);
            11
        }
    })
}

/// [clap4] The command line, from its definition in clap 2's YAML schema
/// (i18n/<lang>_clc.yaml): clap 4 reads no YAML. The keys the definitions use and
/// no others - an unknown one is an error, which the tests meet for every language,
/// rather than dropped. Read as clap 2 read them: a subcommand's `help` is its whole
/// help text, and an argument takes a value when it says so, or has a default or a
/// list of possible values.
fn command(yaml: &str) -> Result<Command, String> {
    let docs = YamlLoader::load_from_str(yaml).map_err(|e| format!("command line: {}", e))?;
    let y = docs.first().ok_or_else(|| "command line: empty".to_string())?;
    let mut cmd = Command::new(text(&y["name"]).unwrap_or_else(|| "sge".to_string()))
        .version(env!("CARGO_PKG_VERSION"));
    for (k, v) in map(y, "command line")? {
        match k.as_str() {
            "name" | "version" | "subcommands" | "args" => {}
            "author" => cmd = cmd.author(text(v).unwrap_or_default()),
            "about" => cmd = cmd.about(text(v).unwrap_or_default()),
            "usage" => cmd = cmd.override_usage(text(v).unwrap_or_default()),
            _ => return Err(format!("command line: unknown key {}", k)),
        }
    }
    for (name, sub) in entries(&y["subcommands"])? {
        let mut c = Command::new(name.clone());
        for (k, v) in map(sub, &name)? {
            match k.as_str() {
                "about" => c = c.about(text(v).unwrap_or_default()),
                "help" => c = c.override_help(text(v).unwrap_or_default()),
                _ => return Err(format!("subcommand {}: unknown key {}", name, k)),
            }
        }
        cmd = cmd.subcommand(c);
    }
    for (name, a) in entries(&y["args"])? {
        let mut arg = Arg::new(name.clone());
        let mut value = false;
        for (k, v) in map(a, &name)? {
            match k.as_str() {
                "help" => arg = arg.help(text(v).unwrap_or_default()),
                "long" => arg = arg.long(text(v).unwrap_or_default()),
                "short" => arg = arg.short(text(v).and_then(|s| s.chars().next())
                    .ok_or_else(|| format!("argument {}: short is one character", name))?),
                "takes_value" => value |= v.as_bool() == Some(true),
                "default_value" => {
                    value = true;
                    arg = arg.default_value(text(v).unwrap_or_default());
                }
                "possible_values" => {
                    value = true;
                    let vs = v.as_vec().ok_or_else(|| format!("argument {}: possible_values is a list", name))?;
                    arg = arg.value_parser(PossibleValuesParser::new(vs.iter().filter_map(text)));
                }
                _ => return Err(format!("argument {}: unknown key {}", name, k)),
            }
        }
        cmd = cmd.arg(arg.action(if value { ArgAction::Set } else { ArgAction::SetTrue }));
    }
    Ok(cmd)
}

/// a scalar as text: a number or a boolean as it is written
fn text(y: &Yaml) -> Option<String> {
    match y {
        Yaml::String(s) | Yaml::Real(s) => Some(s.clone()),
        Yaml::Integer(i) => Some(i.to_string()),
        Yaml::Boolean(b) => Some(b.to_string()),
        _ => None,
    }
}

/// a mapping's keys and values, in their order
fn map<'a>(y: &'a Yaml, what: &str) -> Result<Vec<(String, &'a Yaml)>, String> {
    let h = y.as_hash().ok_or_else(|| format!("{}: not a mapping", what))?;
    h.iter().map(|(k, v)| text(k).map(|k| (k, v)).ok_or_else(|| format!("{}: a key that is not text", what)))
        .collect()
}

/// a list of one-key mappings, `- name: {...}`: clap 2's subcommands and args
fn entries(y: &Yaml) -> Result<Vec<(String, &Yaml)>, String> {
    match y {
        Yaml::BadValue => Ok(vec![]), // not there
        Yaml::Array(a) => a.iter().map(|e| {
            let m = map(e, "an entry")?;
            match m.as_slice() {
                [(k, v)] => Ok((k.clone(), *v)),
                _ => Err("an entry names one subcommand or argument".to_string()),
            }
        }).collect(),
        _ => Err("subcommands and args are lists".to_string()),
    }
}

/// What the command line itself set, not a default - clap 2's occurrences_of > 0;
/// a flag's value is "", as clap 2's value_of gave none.
fn given(cli: &Command, m: &ArgMatches, id: &str) -> Option<String> {
    if !cli.get_arguments().any(|a| a.get_id().as_str() == id) || m.value_source(id) != Some(ValueSource::CommandLine) {
        return None;
    }
    Some(m.try_get_one::<String>(id).ok().flatten().cloned().unwrap_or_default())
}

async fn act(cmd: &str, cfg: ConfigMap, ) -> Result<(), String> {
    #[cfg(debug_assertions)] if cfg.get_bool("verbose", false) {
        cfg.debug(&cfg.log);
    }
    let log = cfg.log.clone();
    match cmd {
        "migrate" => sge::migrate(cfg, false, &log).await,
        "prepare" => sge::migrate(cfg, true, &log).await,
        "snapshot" => sge::load(true, cfg, &log).await,
        "info" => sge::load(false, cfg, &log).await,
        c @ _ => Err(sge::TR.t7ea("cl", "unknown", &["{cmd}", "{$usage_hint}"], &[&c.to_string()])),
    }
}

/// [banner] The DBinvent logo in ASCII, drawn after dbinvent.com's: a D of D's and a
/// B of B's, and "invent" under them in smaller lowercase letters of their own
const LOGO: [&str; 16] = [
    "DDDDDDDDDDDD     BBBBBBBBBBBBB  ",
    "DDDDDDDDDDDDDD   BBBBBBBBBBBBBB ",
    "DDDDD    DDDDDD  BBBBB    BBBBB ",
    "DDDDD     DDDDD  BBBBB    BBBBB ",
    "DDDDD     DDDDD  BBBBBBBBBBBBB  ",
    "DDDDD     DDDDD  BBBBB     BBBBB",
    "DDDDD    DDDDDD  BBBBB     BBBBB",
    "DDDDDDDDDDDDDD   BBBBBBBBBBBBBBB",
    "DDDDDDDDDDDD     BBBBBBBBBBBBBB ",
    "                                ",
    "i                            t  ",
    "                             t  ",
    "i  nnn   v   v   ee   nnn    ttt",
    "i  n  n  v   v  eeee  n  n   t  ",
    "i  n  n   v v   e     n  n   t  ",
    "i  n  n    v     eee  n  n    tt",
];

/// [banner] The product and its version, "Powered by" the logo, and the slogan under
/// it - nothing beside the logo
fn banner() -> String {
    let mut b = format!("Schema Guard Enterprise {}\nPowered by\n\n", env!("CARGO_PKG_VERSION"));
    for l in LOGO.iter() {
        b.push_str(l.trim_end());
        b.push('\n');
    }
    b.push_str("\nThe infinite-scale Postgres cluster and app server - check DBinvent.com\n");
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_shows_the_product_the_logo_and_the_slogan() {
        let b = banner();
        let lines: Vec<&str> = b.lines().collect();
        assert_eq!(lines[0], format!("Schema Guard Enterprise {}", env!("CARGO_PKG_VERSION")), "{}", b);
        assert_eq!(lines[1], "Powered by", "{}", b);
        assert!(b.contains("DDDDD") && b.contains("BBBBB") && b.contains("nnn"), "{}", b);
        assert!(b.trim_end().ends_with("Postgres cluster and app server - check DBinvent.com"), "{}", b);
        // every logo row a line of its own: nothing printed beside the logo
        assert!(LOGO.iter().all(|l| l.len() == LOGO[0].len()), "{:?}", LOGO);
        assert!(LOGO.iter().all(|l| lines.contains(&l.trim_end())), "{}", b);
    }

    #[test]
    fn lazy_test() {
        assert!(sge::CONFIG_APP.len() > 15);
        assert_eq!(sge::STATEMENT_LOG_TBL.table_name, "sge_history");
        assert!(sge::STATEMENT_LOG_TBL.columns.len() > 15);
    }

    #[test]
    fn config_test() {
        // every language's definition reads, with nothing unknown, into a command
        // clap itself finds consistent
        for l in ["en", "ru"] {
            let c = command(sge::i18n::cl_config_for(l)).unwrap_or_else(|e| panic!("{}: {}", l, e));
            assert!(c.get_name().len() > 0);
            assert!(c.get_arguments().count() > 15, "{}", l);
            c.debug_assert();
        }
    }

    #[test]
    fn the_command_line_gives_what_was_set_not_the_defaults() {
        let cli = command(sge::i18n::cl_config_for("en")).unwrap();
        let m = cli.clone().try_get_matches_from(["sge", "--db_name", "app", "-c", "x.cfg", "--logdb", "primary", "migrate"])
            .unwrap();
        assert_eq!(m.subcommand_name(), Some("migrate"));
        assert_eq!(given(&cli, &m, "db_name").as_deref(), Some("app"));
        assert_eq!(given(&cli, &m, "config_file").as_deref(), Some("x.cfg"), "the short -c");
        assert_eq!(given(&cli, &m, "logdb").as_deref(), Some("primary"));
        assert_eq!(given(&cli, &m, "db_host"), None, "a default is not the command line's");
        assert_eq!(m.get_one::<String>("db_host").map(String::as_str), Some("/var/run/postgresql"));
        assert_eq!(given(&cli, &m, "no_such_option"), None);
        assert!(cli.clone().try_get_matches_from(["sge", "--logdb", "neither", "migrate"]).is_err(),
            "a value outside possible_values");
        assert_eq!(cli.get_version(), Some(env!("CARGO_PKG_VERSION")), "the crate's, not local_build");
    }

    #[test]
    fn an_unknown_key_in_the_definition_is_an_error() {
        let e = command("name: sge\nargs:\n    - x:\n        long: x\n        required: true\n").unwrap_err();
        assert!(e.contains("unknown key required"), "{}", e);
    }

}
