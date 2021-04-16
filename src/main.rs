#[macro_use]
extern crate slog;

use clap::App;
use slog::Logger;
use sloggers::terminal::{Destination, TerminalLoggerBuilder};
use sloggers::types::{Format, Severity};
use sloggers::Build;

use rdbm::dbm_yml::ConfigMap;
use std::collections::HashMap;
use std::fs;

#[inline]
pub fn logger() -> Logger {
    logger2(Severity::Debug)
}

#[inline]
pub fn logger2(level: Severity) -> Logger {
    let mut builder = TerminalLoggerBuilder::new();
    builder.channel_size(1024);
    builder.destination(Destination::Stdout);
    builder.format(Format::Compact);
    builder.level(level);
    builder.build().unwrap()
}

fn main() -> Result<(), String> {
    let log = logger();
    let yaml = clap::YamlLoader::load_from_str(rdbm::CONFIG_DEF_YML).unwrap();
    let app = App::from_yaml(&yaml[0]).get_matches();
    let cmd = match app.subcommand_name() {
        None => {
            println!("{}", app.usage());
            return Ok(());
        }
        Some(c) => c.to_string(),
    };
    let (cfg, file) = if app.occurrences_of("config_file") > 0 {
        let file = app.value_of("config_file").unwrap();
        (
            ConfigMap::new(load(file)),
            format!("app configs loaded from: {} (other params ignored)", file),
        )
    } else {
        (ConfigMap::Cli(app), "".into())
    };
    if cmd == "migrate" {
        info!(log, "Migrating... {} ", file);
        rdbm::migrate(cfg, false, &log)
    } else if cmd == "prepare" {
        info!(log, "Preparing... {}", file);
        rdbm::migrate(cfg, true, &log)
    } else if cmd == "info" {
        info!(log, "Load info schema... {}", file);
        match rdbm::load(cfg, &log) {
            Ok(loaded) => {
                info!(log, "{}", loaded);
                Ok(())
            }
            Err(e) => {
                error!(log, "{}", e);
                Err(e)
            }
        }
    } else {
        Ok(())
    }
}

#[inline]
fn load(filename: &str) -> HashMap<String, String> {
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
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

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
