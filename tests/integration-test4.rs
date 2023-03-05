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
use yaml_rust::YamlLoader;
use crate::init::PgInstance;

/**
Test plan:
same as it1, but with ETL
*/

#[test]
fn migration_tests4() -> Result<(), String> {
    match init::prepare_db() {
        Ok(pg) => {
            let res = run_all(&pg);
            thread::sleep(Duration::from_secs(1));
            init::cleanup_db(pg);
            if let Err(e) = res {
                assert!(false, "Testing: {}", e);
            }
        }
        Err(e) => {
            error!(init::LOG, "ignore prepare error {}", e);
        }
    }

    Ok(())
}

fn run_all(pg: &PgInstance) -> Result<(), String> {
    let mut i = 0usize;
    loop {
        let d = format!("./tests/test_scen/case{}",  i);
        if !Path::new(d.as_str()).is_dir() || i > 100 {
            break;
        }
        i += 1;
        println!("\n\nStarting test case: {}", d);
        migrate(pg.with_src(&d, vec![
            "createdb", i.to_string().as_str(),
        ]), false, &init::LOG)?;
        init::compare_with(&d, "test.result", init::dump_db(&pg)?)?;

        let dumys = format!("{}/dump.yaml", d);
        let mut opts= vec![
            "snapshot_to", dumys.as_str()
        ];
        if i >= 3 { // после того, как добавили test_ref_table в YAML:
            let mut od= vec![
                "snapshot_data", "test_ref_table,test_table",
                "snapshot_version", "2"
            ];
            opts.append(&mut od);
        };

        rdbm::load(true, pg.with_src(&d, opts), &init::LOG)?;
        let dumpys = fs::read_to_string(dumys.as_str()).map_err(|e| format!("reading [{}] {}", dumys, e))?;
        if i < 3 {
            init::compare_with(&d, "test.result.yaml", dumpys)?;
        } else {
            init::compare_with(&d, "test.result.dump.yaml", dumpys)?;
            init::diff(&d, "D2__test_schema_test_table.csv")?;
            init::diff(&d, "D2__test_schema_test_ref_table.csv")?;
        }
    }

    info!(init::LOG, "\nre-migrate case[{}]", i);
    migrate(pg.with_src(&format!("./tests/test_scen/case{}",  i-1), vec![]), false, &init::LOG)?;

    Ok(())
}
