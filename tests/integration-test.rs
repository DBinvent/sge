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
use yaml_rust2::YamlLoader;
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

#[tokio::test]
async fn migration_tests() -> Result<(), String> {
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
    let mut i = 0usize;
    loop {
        let d = format!("./tests/test_scen/case{}",  i);
        if !Path::new(d.as_str()).is_dir() {
            return Ok(());
        }
        if i > 100 {
            break;
        }
        i += 1;
        info!(init::LOG, "\n\nStarting test case: {}", d);
        migrate(pg.with_src(&d, vec!["createdb", i.to_string().as_str()]), false, &init::LOG).await?;
        init::compare_with(&d, "test.result", init::dump_db(&pg)?)?;

        let dumys = format!("{}/dump.yaml", d);
        sge::load(true, pg.with_src(&d, vec!["snapshot_to", dumys.as_str()]), &init::LOG).await?;
        let dumpys = fs::read_to_string(dumys.as_str()).map_err(|e| format!("reading [{}] {}", dumys, e))?;
        init::compare_with(&d, "test.result.yaml", dumpys)?;

    }
    Ok(())
}
