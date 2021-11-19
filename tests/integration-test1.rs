#![allow(unused, warnings)]

extern crate rdbm;

mod init;

#[macro_use]
extern crate slog;

use std::path::Path;
use std::{env, fs, thread};
use rdbm::{migrate, load};
use rdbm::dbm_cfg::ConfigMap;
use slog::Logger;
use std::time::Duration;
use std::env::VarError;
use crate::init::PgInstance;

/**
Test plan:
1. logdb on other db, schema, table (done in it2)
2. create db, role, schema, check owner
3. create table (sql, constraint), column (sql) check null, defaultValue, owner
4. alter column: add fk to table in another schema
5. yaml dump compare
(see it1)
6. check version destination for P/S/V/R, multiple schema files for same table
7. check filter: target, catalog, script_tag for P/S/V/R info action

8. check.1 with tsl
*/

#[test]
fn migration_ver_tests1() -> Result<(), String> {
    match init::prepare_db("/tmp/tests", "test") {
        Ok(pg) => {
            let res = run_all(&pg);
            thread::sleep(Duration::from_secs(1));
            init::cleanup_db(pg);
            if let Err(e) = res {
                assert!(false, "Testing: {}", e);
            }
        }
        Err(e) => {
            println!("ignore prepare error");
            eprintln!("{}", e);
        }
    }

    Ok(())
}

fn run_all(pg: &PgInstance) -> Result<(), String> {
    let d = "./tests/test_scen".to_string();
    run(1, &d, pg)?;
    run(2, &d, pg)
}

fn run(ver: usize, d: &String, pg: &PgInstance) -> Result<(), String> {
    let verf = format!("{}/ver.out", d);
    let mut cfg = pg.with_src(&d,
        vec!["createdb",  ver.to_string().as_str(),
            "target", format!("1.{}", ver).as_str(),
            // "pre_deploy".to_string(), format!("1.{}", ver).as_str(),
            // "schema_deploy".to_string(), format!("1.{}", ver).as_str(),
            // "version_deploy".to_string(), format!("1.{}", ver).as_str(),
            // "rr_deploy".to_string(), format!("1.{}", ver).as_str(),
            "script_tag", format!("tag_1_{}", ver).as_str(),
            "executed_by", format!("dba{}", ver).as_str(),
        ]);

    println!("\n\nStarting test case ver[1.{}] from {}", ver, d);

    migrate(cfg.clone(), false, &init::LOG)?;
    init::compare_with(&format!("{}/case{}", d, ver), "test.result", init::dump_db(&pg)?)?;

    let _ = load(false, cfg.clone(),&init::LOG)?;
    cfg.insert(&"snapshot_to".to_string(), verf.as_str());
    cfg.insert(&"out_format".to_string(), "json");
    let _ = load(false, cfg.clone(),&init::LOG)?;
    let sfv = fs::read_to_string(&verf).map_err(|e| format!("{} {}", verf,  e))?;
    let vvv  = "{\"\":{\"\":{\"version\":\"1.%\",\"successful\":true,\"script_tag\":\"tag_1_%\",\"executed_by\":\"dba%\",";
    let vvv = vvv.replace("%", ver.to_string().as_str());
    assert!(sfv.starts_with(vvv.as_str()), "{} \n<> \n{}", sfv, vvv);

    Ok(())
}
