// # [macro_use]
extern crate slog;

use clap::App;
use slog::Logger;
use sloggers::terminal::{Destination, TerminalLoggerBuilder};
use sloggers::types::{Format, Severity};
use sloggers::Build;

use rdbm::dbm_cfg::ConfigMap;

#[inline]
pub fn logger(debug: bool) -> Logger {
    let mut builder = TerminalLoggerBuilder::new();
    builder.channel_size(1024);
    builder.destination(Destination::Stdout);
    builder.format(Format::Compact);
    builder.level(if debug {
        Severity::Debug
    } else {
        Severity::Info
    });
    builder.build().unwrap()
}

fn main() -> Result<(), String> {
    let yaml = clap::YamlLoader::load_from_str(rdbm::CONFIG_DEF_YML).unwrap();
    let app = App::from_yaml(&yaml[0]).get_matches();
    let cmd = match app.subcommand_name() {
        Some(cmd) => cmd.to_owned(),
        None => {
            println!("{}", app.usage());
            return Ok(());
        }
    };
    let mut cfg = ConfigMap::load(if app.occurrences_of("config_file") > 0 {
        app.value_of("config_file").unwrap_or("")
    } else {
        ""
    })?;
    for ks in rdbm::CONFIG_APP.keys() {
        if app.occurrences_of(ks.as_str()) > 0 {
            cfg.insert(ks, app.value_of(ks).unwrap_or(""));
        }
    }
    let log = logger(cfg.get_bool("verbose", false));

    match cmd.as_str() {
        "migrate" => {
            if rdbm::license::is_ok(&cfg, &log)? {
                rdbm::migrate(cfg, false, &log)
            } else {
                Err(format!("Invalid license"))
            }
        }
        "prepare" => rdbm::migrate(cfg, true, &log),
        "snapshot" => rdbm::load(true, cfg, &log),
        "info" => rdbm::load(false, cfg, &log),
        "license" => rdbm::license::license(&cfg, &log, None),
        c @ _ => Err(format!("Unknown command: {}", c)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lazy_test() {
        assert!(rdbm::CONFIG_APP.len() > 15);
        assert_eq!(rdbm::STATEMENT_LOG_TBL.table_name, "rdbm_history");
        assert!(rdbm::STATEMENT_LOG_TBL.columns.len() > 15);
    }

    #[test]
    fn config_test() {
        match clap::YamlLoader::load_from_str(rdbm::CONFIG_DEF_YML) {
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
