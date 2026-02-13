extern crate slog;

use clap::App;

use rdbm::dbm_cfg::ConfigMap;

fn main() {
    let yaml = clap::YamlLoader::load_from_str(rdbm::i18n::cl_config()).unwrap();
    let app = App::from_yaml(&yaml[0]).get_matches();
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
        Err(e) => { println!("{}", e); std::process::exit(2); }
    };
    for ks in rdbm::CONFIG_APP.keys() {
        // println!("check [{}]= [{}] {} ", ks, app.occurrences_of(ks.as_str()), app.value_of(ks).unwrap_or("UNSET"));
        if app.occurrences_of(ks.as_str()) > 0 {
            cfg.insert(ks, app.value_of(ks).unwrap_or(""));
        }
    }
    std::process::exit(match act (cmd.as_str(), cfg) {
        Ok(_) => 0,
        Err(e) => {
            println!("{}", e);
            11
        }
    })
}

fn act(cmd: &str, cfg: ConfigMap, ) -> Result<(), String> {
    #[cfg(debug_assertions)] if cfg.get_bool("verbose", false) {
        cfg.debug(&cfg.log);
    }
    let log = cfg.log.clone();
    match cmd {
        "migrate" => rdbm::migrate(cfg, false, &log),
        "prepare" => rdbm::migrate(cfg, true, &log),
        "snapshot" => rdbm::load(true, cfg, &log),
        "info" => rdbm::load(false, cfg, &log),
        c @ _ => Err(rdbm::TR.t7ea("cl", "unknown", &["{cmd}"], &[&c.to_string()])),
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
        match clap::YamlLoader::load_from_str(rdbm::i18n::cl_config()) {
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
