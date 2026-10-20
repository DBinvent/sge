#![forbid(unsafe_code)]
extern crate slog;

use clap::App;

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
    let yaml = clap::YamlLoader::load_from_str(sge::i18n::cl_config()).unwrap();
    // the version is the crate's, not the yaml's "local_build" placeholder, so every
    // build reports its own (help2man reads it into the man page too)
    let app = App::from_yaml(&yaml[0]).version(env!("CARGO_PKG_VERSION")).get_matches();
    let cmd = match app.subcommand_name() {
        Some(cmd) => cmd.to_owned(),
        None => {
            println!("{}", app.usage());
            std::process::exit(0);
        }
    };
    let mut cfg = match ConfigMap::load(if app.occurrences_of("config_file") > 0 {
        app.value_of("config_file").unwrap_or("")
    } else {
        ""
    }) {
        Ok(c) => c,
        Err(e) => { eprintln!("{}", e); std::process::exit(2); }
    };
    for ks in sge::CONFIG_APP.keys() {
        if app.occurrences_of(ks.as_str()) > 0 {
            cfg.insert(ks, app.value_of(ks).unwrap_or(""));
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
        match clap::YamlLoader::load_from_str(sge::i18n::cl_config()) {
            Ok(y) => {
                assert_eq!(y.len(), 1);
                let m = App::from_yaml(&y[0]);
                assert!(m.get_name().len() > 0);
            }
            Err(e) => {
                assert!(false, "{}", e);
            }
        }
    }

}
