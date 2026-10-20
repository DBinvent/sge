#![allow(unused, warnings)]

extern crate sge;

mod init;

#[macro_use]
extern crate slog;

use std::path::Path;
use std::{env, fs, thread};
use sge::migrate;
use sge::dbm_cfg::ConfigMap;
use slog::Logger;
use std::time::Duration;
use std::env::VarError;

use crate::init::PgInstance;

/**
Test plan:
1. logdb COPY on other db, schema, table
TODO create empty sge_history table on logdb to simulate it upgrade
*/

#[tokio::test]
async fn migration_log_db_tests3() -> Result<(), String> {
    match init::prepare_db() {
        Ok(pg) => {
            match init::prepare_db() {
                Ok(logpg) => {
                    let mut pg = pg.clone();
                    pg.cfg.insert(&"logdb_host".to_string(), "localhost");
                    pg.cfg.insert(&"logdb_port".to_string(), logpg.port.to_string().as_str());
                    pg.cfg.insert(&"logdb_name".to_string(), logpg.db_name.as_str());
                    pg.cfg.insert(&"logdb_user".to_string(), env::var("USER").unwrap().as_str());
                    pg.cfg.insert(&"logdb_password".to_string(), logpg.db_pwd.as_str());
                    pg.cfg.insert(&"createlogdb_user".to_string(), env::var("USER").unwrap().as_str());
                    pg.cfg.insert(&"createlogdb_pwd".to_string(), logpg.db_pwd.as_str());

                    info!(init::LOG, "starting split logging cases");
                    thread::sleep(Duration::from_secs(1));
                    let res = run_all(&pg, &logpg).await;

                    thread::sleep(Duration::from_secs(1));
                    init::cleanup_db(logpg);
                    init::cleanup_db(pg);
                    if let Err(e) = res {
                        assert!(false, "Testing: {}", e);
                    }
                }
                Err(e) => {
                    init::prepare_failed("the log database", &e);
                }
            }
        }
        Err(e) => {
            init::prepare_failed("the main database", &e);
        }
    }
    Ok(())
}

/// wrapping errors
async fn run_all(pg: &PgInstance, logpg: &PgInstance) -> Result<(), String> {
    let d0 = "./tests/test_scen/case0".to_string();
    migrate(pg.with_src(&d0, vec!["createdb", "yes"]), false, &init::LOG).await?;
    init::compare_with(&d0, "test.result", init::dump_db(&logpg)?)?;
    // fs::write(format!("{}/empty.result", &d0), "");
    init::compare_with(&d0, "test.result", init::dump_db(&pg)?)?;

    let d1 = "./tests/test_scen/case1".to_string();
    migrate(pg.with_src(&d1, vec![]), false, &init::LOG).await?;
    init::compare_with(&d1, "test-lc-db.result", init::dump_db(&pg)?)?;
    init::compare_with(&d0, "test.result", init::dump_db(&logpg)?)?;

    Ok(())
}