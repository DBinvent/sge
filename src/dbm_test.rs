use std::result::Result;
use std::collections::HashMap;
use serde::Deserialize;

use lazy_static::lazy_static;
use std::fmt::Display;
use std::{fmt, fs};
use postgres::{Transaction, Client, Error, Row};
use std::collections::hash_map::RandomState;
use crate::dbm_yml::{ConfigDefMap, YTable, ConfigMap};
use postgres::types::Timestamp;
// use serde::private::ser::constrain;
use crate::BASELINE;
use slog::Logger;
use std::process::{Command, Output};
use std::io::{Read, Write};
use base64::DecodeError;
use crypto_hash::Algorithm;
use std::fs::File;
// use sha1::Sha1;


#[cfg(test)]
mod tests {
	use super::*;
	use postgres::NoTls;
	use crate::connect;
    use sloggers::types::Severity;
    use sloggers::terminal::{TerminalLoggerBuilder, Destination};
	use sloggers::Build;
    use crate::dbm_dbc::STable;

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

	#[test]
	// TODO FIXME NOW REMOVE PWD
	fn cmd_run_test() {
		let a = vec![1,2];
		let b = vec![4,5];
		assert_ne!(a, b);

		let log = logger();
        let mut cfg = ConfigMap::new("db_port", "5433");
        cfg.set("db_name", "ucb");
        cfg.set("db_user", "vk");
        cfg.set("db_password", "1");
        cfg.set("db_host", "localhost");
        /*
        match connect("db", &cfg) {
            Ok(mut c) => {
            	let mut t = c.transaction().unwrap();
            	match StatementLog::deploy(&HashMap::new(), "/tmp/.rdbm.conf", &cfg, 'V',
            		&"1.1".to_string(), &"./tests/scripts/V1_1__sample_ddl.sql".to_string(),
            		&mut t, &log ) {
					Ok(_) => {
						assert!(true);
					}
					Err(e) => {assert!(false, "{}", e);}
				}
            }
            Err(e) => {
                assert!(false, "{}", e);
            }
        }*/
	}

    #[test]
    fn connect_test() {
        let mut cfg = ConfigMap::new("db_port", "5433");
        cfg.set("db_name", "vk");
        cfg.set("db_user", "vk");
        match connect("db", &cfg) {
            Ok(mut c) => {
                let dbc = crate::dbm_dbc::load_info_schema(&mut c, &cfg).unwrap();
                assert!(dbc.len() > 0);
                match dbc.get("public") {
                    None => { assert!(false); }
                    Some(p) => {
                        assert!(p.len() > 0);
                    }
                }
                println!("{:?}", dbc);
            }
            Err(e) => {
                assert!(false, "{}", e);
            }
        }
    }


}
