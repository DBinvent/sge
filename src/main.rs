#[macro_use]
extern crate slog;

use clap::{App, ArgMatches};
use slog::Logger;
use sloggers::terminal::{Destination, TerminalLoggerBuilder};
use sloggers::types::{Format, Severity};
use sloggers::Build;

use rdbm::dbm_yml::ConfigMap;
use std::collections::HashMap;
use std::fs;

mod license;

#[inline]
pub fn logger(debug: bool) -> Logger {
    let mut builder = TerminalLoggerBuilder::new();
    builder.channel_size(1024);
    builder.destination(Destination::Stdout);
    builder.format(Format::Compact);
    builder.level(if debug {Severity::Debug} else {Severity::Info});
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
    let cfg = if app.occurrences_of("config_file") > 0 {
        let file = app.value_of("config_file").unwrap();
        ConfigMap::new(load(file, &app))
    } else {
        ConfigMap::Cli(app)
    };
    let log = logger(cfg.get_bool("verbose", false));

    match cmd.as_str() {
        "migrate" => {
            if license::is_ok(&cfg, &log)? {
                rdbm::migrate(cfg, false, &log)
            } else {
                Err(format!("Invalid license"))
            }
        }
        "prepare" => rdbm::migrate(cfg, true, &log),
        "snapshot" => rdbm::load(true, cfg, &log),
        "info" => rdbm::load(false, cfg, &log),
        "license" => license::license(cfg, &log),
        c @ _ => Err(format!("Unknown command: {}", c)),
    }
}

#[inline]
fn load(filename: &str, app: &ArgMatches) -> HashMap<String, String> {
    let mut cfg = HashMap::new();
    match fs::read_to_string(filename) {
        Ok(data) => {
            let ref ym = clap::YamlLoader::load_from_str(data.as_str()).expect("wrong format")[0];
            let ymh = ym.as_hash().unwrap();
            for (k, v) in ymh {
                if let Some(ks) = k.as_str() {
                    if let Some(vs) = v.as_str() {
                        cfg.insert(ks.into(), vs.into());
                    }
                }
            }
        }
        Err(e) => {
            panic!("loading {} {}", filename, e);
        }
    }
    for p in rdbm::CONFIG_YAML["args"].as_vec().unwrap() {
        let ymh = p.as_hash().unwrap();
        for (k, _) in ymh {
            if let Some(ks) = k.as_str() {
                if app.occurrences_of(ks) == 1 {
                    cfg.insert(ks.to_string(), app.value_of(ks).unwrap().into());
                }
            }
        }
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lazy_test() {
        assert!(rdbm::CONFIG_YAML["args"].as_vec().unwrap().len() > 15);
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
