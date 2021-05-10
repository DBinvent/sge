#![allow(unused, warnings)]

extern crate rdbm;

mod init;

#[macro_use]
extern crate slog;

use std::path::Path;
use std::{env, fs, thread};
use rdbm::migrate;
use rdbm::dbm_cfg::ConfigMap;
use slog::Logger;
use std::time::Duration;
use std::env::VarError;

pub const PG_VER: &str = "12";


/**
Test plan:
0.
prepare:
check postgres
initdb, up it

x. run migration, dump, compare

9.
cleanup

*/

#[test]
fn migration_tests() -> Result<(), String> {
    // let log = init::logger(true);
    match env::var("RDBM_IT") {
        Ok(v) => {
            println!("migration_tests: {}", v);
            if v.as_str() == "all" {
                let v = env::var("PG_VER")
                    .unwrap_or(PG_VER.to_string())
                    .parse::<u8>()
                    .map_err(|e| format!("Parsing env PG_VER for testing: {} (default is {})", e, PG_VER))
                    .unwrap();

                match init::prepare_db("/tmp/tests", "test", v) {
                    Ok((work_dir, port, db_name, db_pwd)) => {
                        let res = run_all(&work_dir, port, &db_name, v, db_pwd, &init::LOG);
                        thread::sleep(Duration::from_secs(1));
                        init::cleanup_db(work_dir, port, db_name, v);
                        if let Err(e) = res {
                            assert!(false, "Testing: {}", e);
                        }
                    }
                    Err(e) => {
                        println!("ignore prepare error");
                        eprintln!("{}", e);
                    }
                }
            }
        }
        Err(_) => {}
    }
    Ok(())
}

fn run_all(work_dir: &String, port: u16, db_name: &String, v: u8, db_pwd: String, log: &Logger) -> Result<(), String> {
    let mut i = 0usize;
    loop {
        let d = format!("./tests/test_scen/case{}",  i);
        if !Path::new(d.as_str()).is_dir() {
            return Ok(());
        }
        println!("\n\nStarting: {}", d);
        let cf = format!("{}/test.cfg", d);
        let mut cf = ConfigMap::load(cf.as_str())?;
        cf.insert(&"createdb_user".to_string(), env::var("USER").unwrap().as_str());
        cf.insert(&"createdb_pwd".to_string(), db_pwd.as_str());
        cf.insert(&"db_name".to_string(), db_name.as_str());
        cf.insert(&"db_user".to_string(), env::var("USER").unwrap().as_str());
        cf.insert(&"db_password".to_string(), db_pwd.as_str());
        cf.insert(&"source_dir".to_string(), d.as_str());
        cf.insert(&"db_port".to_string(), port.to_string().as_str());
        migrate(cf, false, log)?;
        init::compare_with(d, init::dump_db(work_dir, port, db_name, v)?)?;
        i += 1;
    }
    Ok(())
}
