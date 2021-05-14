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
use crate::init::PgInstance;

/**
Test plan:
1. logdb on other db, schema, table (done  in it2)
2. create db, role, schema, check owner
3. create table (sql, constraint), column (sql) check null, defaultValue, owner
4. alter column: add fk to table in another schema
5. yaml dump compare
6. check version destination for P/S/V/R, multiple schema files for same table
7. check filter: target, catalog, script_tag for P/S/V/R info action
*/

#[test]
fn migration_tests() -> Result<(), String> {
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
    let mut i = 0usize;
    loop {
        let d = format!("./tests/test_scen/case{}",  i);
        if !Path::new(d.as_str()).is_dir() {
            return Ok(());
        }
        i += 1;
        println!("\n\nStarting test case: {}", d);
        migrate(pg.with_src(&d, vec!["createdb", i.to_string().as_str()]), false, &init::LOG)?;
        init::compare_with(d.clone(), "test.result", init::dump_db(&pg)?)?;

        let dumys = format!("{}/dump.yaml", d).as_str();
        rdbm::load(true, pg.with_src(&d, vec!["snapshot_to", dumys]), &init::LOG)?;
        let dumpys = fs::read_to_string(dumys).map_err(|e| format!("reading [{}] {}", dumpus, e))?;
        init::compare_with(d, "test.result.yaml", dumpys)?;

    }
    Ok(())
}
