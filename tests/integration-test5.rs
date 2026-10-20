#![allow(unused, warnings)]

extern crate sge;

mod init;

#[macro_use]
extern crate slog;

use std::path::Path;
use std::{env, fs, thread};
use sge::{migrate, load};
use sge::dbm_cfg::ConfigMap;
use slog::Logger;
use std::time::Duration;
use std::env::VarError;
use crate::init::PgInstance;

/**
Test plan:

same as it1 with schema retry

*/

#[tokio::test]
async fn migration_ver_tests1() -> Result<(), String> {
    match init::prepare_db() {
        Ok(pg) => {
            let res = run_all(&pg).await;
            thread::sleep(Duration::from_secs(1));
            init::cleanup_db(pg);
            if let Err(e) = res {
                assert!(false, "Testing: {}", e);
            }
        }
        Err(e) => {
            init::prepare_failed("the database", &e);
        }
    }

    Ok(())
}

async fn run_all(pg: &PgInstance) -> Result<(), String> {
    let d = "./tests/test_scen".to_string();
    run(1, &d, &pg).await?;
    run(2, &d, pg).await
}

async fn run(ver: usize, d: &String, pg: &PgInstance) -> Result<(), String> {
    let verf = format!("{}/ver.out", d);
    let mut cfg = pg.with_src(&d,
        vec!["createdb",  ver.to_string().as_str(),
            "target", format!("1.{}", ver).as_str(),
            // "pre_deploy".to_string(), format!("1.{}", ver).as_str(),
            // "schema_deploy".to_string(), format!("1.{}", ver).as_str(),
            // "version_deploy".to_string(), format!("1.{}", ver).as_str(),
            "schema_transaction", "retry",
            "script_tag", format!("tag_1_{}", ver).as_str(),
            "executed_by", format!("dba{}", ver).as_str(),
            "etl", "ignore" //"skip",
        ]);

    info!(init::LOG, "\n\nStarting test case ver[1.{}] from {} with: schema_transaction=retry", ver, d);

    migrate(cfg.clone(), false, &init::LOG).await?;
    init::compare_with(&format!("{}/case{}", d, ver), "test.result", init::dump_db(&pg)?)?;

    let _ = load(false, cfg.clone(), &init::LOG).await?;
    cfg.insert(&"snapshot_to".to_string(), verf.as_str());
    cfg.insert(&"out_format".to_string(), "json");
    let _ = load(false, cfg.clone(), &init::LOG).await?;
    let sfv = fs::read_to_string(&verf).map_err(|e| format!("{} {}", verf,  e))?;
    let vvv  = "{\"\":{\"\":{\"version\":\"1.%\",\"successful\":true,\"script_tag\":\"tag_1_%\",\"executed_by\":\"dba%\",";
    let vvv = vvv.replace("%", ver.to_string().as_str());
    assert!(sfv.starts_with(vvv.as_str()), "{} \n<> \n{}", sfv, vvv);

    Ok(())
}
