#[macro_use]
extern crate clap;
#[macro_use]
extern crate slog;

use clap::{App, YamlLoader};
use slog::{Drain, Logger};
use sloggers::Build;
use sloggers::terminal::{Destination, TerminalLoggerBuilder};
use sloggers::types::Severity;

use rdbm::dbm_yml::{ConfigMap, history_table_name};

#[inline]
pub fn logger() -> Logger {
    logger2(Severity::Info)
}

#[inline]
pub fn logger2(level: Severity) -> Logger {
    let mut builder = TerminalLoggerBuilder::new();
    builder.channel_size(1024);
    builder.destination(Destination::Stdout);
    builder.level(level);
    builder.build().unwrap()
}

fn main() {
    let log = logger();
    let yaml = clap::YamlLoader::load_from_str(rdbm::CONFIG_DEF_YML).unwrap();
    let app = App::from_yaml(&yaml[0]).get_matches();
    if let Some(a) = app.subcommand_matches("migrate") {
        info!(log, "Migrating ...");
        rdbm::migrate(ConfigMap::Cli(app), false, &log);
    } else if let Some(a) = app.subcommand_matches("prepare") {
        info!(log, "Preparing ...");
        rdbm::migrate(ConfigMap::Cli(app), true, &log);
    } else if let Some(a) = app.subcommand_matches("info") {
        info!(log, "Info ...");
        match rdbm::load(ConfigMap::Cli(app), &log) {
            Ok(loaded) => {
                info!(log, "{}", loaded);
            }
            Err(e) => {
                error!(log, "{}", e)
            }
        }

    } else {
        println!("{}", app.usage());
    }
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