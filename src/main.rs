#![forbid(unsafe_code)]
extern crate slog;

use clap::builder::PossibleValuesParser;
use clap::parser::ValueSource;
use clap::{Arg, ArgAction, ArgMatches, Command};
use yaml_rust2::{Yaml, YamlLoader};

use sge::dbm_cfg::ConfigMap;
use sge::i18n::message;

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
    std::process::exit(run(std::env::args_os()).await)
}

/// The command line `args` run - the program's name first - to the exit code: 0
/// done, 2 a command line or configuration file that does not read, 11 the command
/// failed. Apart from main, which exits with it, so the tests can run it.
async fn run<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    // the version is the crate's, not the yaml's "local_build" placeholder, so every
    // build reports its own (help2man reads it into the man page too)
    let cli = match command(sge::i18n::cl_config()) {
        Ok(c) => c,
        Err(e) => { eprintln!("{}", e); return 2; }
    };
    // as get_matches(): --help and --version print and end with 0, a usage error with 2
    let app = match cli.clone().try_get_matches_from(args) {
        Ok(m) => m,
        Err(e) => {
            #[cfg(not(test))]
            let _ = e.print();
            // clap writes to the terminal itself, past a test's capture: a passing test
            // printed "error: unexpected argument" into every cargo test run
            #[cfg(test)]
            if e.use_stderr() { eprint!("{}", e.render()) } else { print!("{}", e.render()) }
            return e.exit_code();
        }
    };
    let cmd = match app.subcommand_name() {
        Some(cmd) => cmd.to_owned(),
        None => {
            println!("{}", cli.clone().render_usage());
            return 0;
        }
    };
    let mut cfg = match ConfigMap::load(given(&cli, &app, "config_file").unwrap_or_default().as_str()) {
        Ok(c) => c,
        Err(e) => { eprintln!("{}", e); return 2; }
    };
    for ks in sge::CONFIG_APP.keys() {
        if let Some(v) = given(&cli, &app, ks) {
            cfg.insert(ks, v.as_str());
        }
    }
    // [superset] a snapshot's ZIP on standard output, [plan] plan's Markdown: the log
    // goes to standard error
    if cmd == "plan" || (cfg.get("out_format").to_lowercase() == "superset" && cfg.get_opt("snapshot_to").is_none()) {
        cfg.log_to_stderr();
    }
    match act(cmd.as_str(), cfg).await {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("{}", e);
            11
        }
    }
}

/// [clap4] The command line, from its definition in clap 2's YAML schema
/// (i18n/<lang>_clc.yaml): clap 4 reads no YAML. The keys the definitions use and
/// no others - an unknown one is an error, which the tests meet for every language,
/// rather than dropped. Read as clap 2 read them: a subcommand's `help` is its whole
/// help text, and an argument takes a value when it says so, or has a default or a
/// list of possible values.
fn command(yaml: &str) -> Result<Command, String> {
    let docs = YamlLoader::load_from_str(yaml).map_err(|e| message("main.yaml", &[("err", &e.to_string())]))?;
    let y = docs.first().ok_or_else(|| "command line: empty".to_string())?;
    let mut cmd = Command::new(text(&y["name"]).unwrap_or_else(|| "sge".to_string()))
        .version(env!("CARGO_PKG_VERSION"));
    for (k, v) in map(y, "command line")? {
        match k.as_str() {
            "name" | "version" | "subcommands" | "args" => {}
            "author" => cmd = cmd.author(text(v).unwrap_or_default()),
            "about" => cmd = cmd.about(text(v).unwrap_or_default()),
            "usage" => cmd = cmd.override_usage(text(v).unwrap_or_default()),
            _ => return Err(message("main.unknown_key", &[("key", &k)])),
        }
    }
    for (name, sub) in entries(&y["subcommands"])? {
        let mut c = Command::new(name.clone());
        for (k, v) in map(sub, &name)? {
            match k.as_str() {
                "about" => c = c.about(text(v).unwrap_or_default()),
                "help" => c = c.override_help(text(v).unwrap_or_default()),
                _ => return Err(message("main.sub_unknown_key", &[("name", &name), ("key", &k)])),
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
                    .ok_or_else(|| message("main.short", &[("name", &name)]))?),
                "takes_value" => value |= v.as_bool() == Some(true),
                "default_value" => {
                    value = true;
                    arg = arg.default_value(text(v).unwrap_or_default());
                }
                "possible_values" => {
                    value = true;
                    let vs = v.as_vec().ok_or_else(|| message("main.possible_values", &[("name", &name)]))?;
                    arg = arg.value_parser(PossibleValuesParser::new(vs.iter().filter_map(text)));
                }
                _ => return Err(message("main.arg_unknown_key", &[("name", &name), ("key", &k)])),
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
    let h = y.as_hash().ok_or_else(|| message("main.not_mapping", &[("what", what)]))?;
    h.iter().map(|(k, v)| text(k).map(|k| (k, v)).ok_or_else(|| message("main.key_not_text", &[("what", what)])))
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
                _ => Err(message("main.one_entry", &[])),
            }
        }).collect(),
        _ => Err(message("main.lists", &[])),
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
        "plan" => sge::plan(cfg, &log).await,
        "snapshot" => sge::load(true, cfg, &log).await,
        "info" => sge::load(false, cfg, &log).await,
        // clap refuses a command it does not define before act() is reached: this is one
        // defined in i18n/*_clc.yaml and not handled here
        c => Err(message("main.unknown_command", &[("cmd", c)])),
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
        for l in ["en", "ru", "fr", "de", "zh", "ja"] {
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
        assert!(e.starts_with("SGE-E025 ") && e.contains("required"), "{}", e);
    }

    /// [i18n] every language's command line is the English one, translated: the same
    /// subcommands and arguments - names, defaults, possible values - and every text of
    /// it translated, so an option added in English alone fails here. Russian, Chinese and
    /// Japanese are told by their script, French and German - Latin, as English is - by
    /// differing from it. The unreviewed ones say so, and where to send a correction
    #[test]
    fn every_command_line_is_the_english_one_translated() {
        let names = |c: &Command| c.get_subcommands().map(|s| s.get_name().to_string()).collect::<Vec<_>>();
        let args = |c: &Command| c.get_arguments()
            .map(|a| (a.get_id().to_string(), a.get_long().map(str::to_string), a.get_short(), a.get_default_values().to_vec(),
                a.get_possible_values().iter().map(|p| p.get_name().to_string()).collect::<Vec<_>>(), format!("{:?}", a.get_action())))
            .collect::<Vec<_>>();
        let about = |c: &Command| c.get_about().map(|t| t.to_string()).unwrap_or_default();
        let texts = |c: &Command| {
            let mut t = vec![("about".to_string(), about(c)), ("usage".to_string(), c.clone().render_usage().to_string())];
            for s in c.get_subcommands() {
                t.push((s.get_name().to_string(), about(s)));
                t.push((format!("{} help", s.get_name()), s.clone().render_help().to_string()));
            }
            for a in c.get_arguments() {
                t.push((a.get_id().to_string(), a.get_help().map(|t| t.to_string()).unwrap_or_default()));
            }
            t
        };
        let en = command(sge::i18n::cl_config_for("en")).unwrap();
        let english = texts(&en);
        for l in ["ru", "fr", "de", "zh", "ja"] {
            let c = command(sge::i18n::cl_config_for(l)).unwrap();
            assert_eq!(names(&en), names(&c), "{}", l);
            assert_eq!(args(&en), args(&c), "{}", l);
            for ((what, e), (_, t)) in english.iter().zip(texts(&c).iter()) {
                let has = |from: char, to: char| t.chars().any(|c| (from..=to).contains(&c));
                let translated = match l {
                    "ru" => has('\u{0400}', '\u{04FF}'),
                    "zh" => has('\u{4E00}', '\u{9FFF}'),
                    "ja" => has('\u{3040}', '\u{30FF}'),
                    _ => t != e,
                };
                assert!(translated, "{} {} is not translated: {}", l, what, t);
            }
            if l != "ru" {
                assert!(about(&c).contains("LANG=en") && about(&c).contains("https://github.com/DBinvent/sge/tree/main/src/i18n"),
                    "{}: no notice that it is not reviewed: {}", l, about(&c));
            }
        }
    }

    #[test]
    fn a_definition_that_does_not_read_is_an_error() {
        let e = |y: &str| command(y).unwrap_err();
        assert!(e("name: sge\nsubcommands:\n    - migrate:\n        alias: m\n").starts_with("SGE-E022 "));
        assert!(e("name: sge\nargs:\n    - x: {long: x}\n      y: {long: y}\n").starts_with("SGE-E028 "));
        assert!(e("name: sge\nargs: {x: {long: x}}\n").starts_with("SGE-E029 "));
        // a number and a boolean read as they are written
        let c = command("name: sge\nargs:\n    - port:\n        long: port\n        default_value: 5432\n\
            \x20   - flag:\n        long: flag\n        default_value: true\n").unwrap();
        let m = c.try_get_matches_from(["sge"]).unwrap();
        assert_eq!(m.get_one::<String>("port").map(String::as_str), Some("5432"));
        assert_eq!(m.get_one::<String>("flag").map(String::as_str), Some("true"));
        assert_eq!(text(&Yaml::Null), None);
    }

    /// [exit] how a run ended: 0 the usage with no command, --help and --version too;
    /// 2 a command line or a configuration file that does not read; 11 a command that
    /// failed - here on a server that is not there
    #[tokio::test]
    async fn the_exit_code_says_how_the_run_ended() {
        assert_eq!(run(["sge"]).await, 0, "no command: the usage");
        assert_eq!(run(["sge", "--help"]).await, 0);
        assert_eq!(run(["sge", "--version"]).await, 0);
        assert_eq!(run(["sge", "--no_such_option", "migrate"]).await, 2);
        assert_eq!(run(["sge", "-c", "/nonexistent/sge.cfg", "migrate"]).await, 2);
        assert_eq!(run(["sge", "--db_host", "127.0.0.1", "--db_port", "1", "--db_name", "none", "info"]).await, 11);
    }

    /// [cmd] each command reaches its run - and fails there on a server that is not
    /// there; a command sge does not know is named in the error
    #[tokio::test]
    async fn each_command_runs_and_an_unknown_one_is_named() {
        let cfg = || {
            let mut c = ConfigMap::load("").unwrap();
            for (k, v) in [("db_host", "127.0.0.1"), ("db_port", "1"), ("db_name", "none"), ("verbose", "true")] {
                c.insert(&k.to_string(), v);
            }
            c
        };
        for cmd in ["migrate", "plan", "prepare", "snapshot", "info"] {
            let e = act(cmd, cfg()).await.unwrap_err();
            assert!(e.starts_with("SGE-E009 "), "{}: {}", cmd, e);
        }
        let e = act("bogus", cfg()).await.unwrap_err();
        assert!(e.starts_with("SGE-E030 ") && e.contains("bogus"), "{}", e);
    }

}
