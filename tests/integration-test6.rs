#![allow(unused, warnings)]

extern crate sge;

mod init;

#[macro_use]
extern crate slog;

use std::{fs, thread};
use std::time::Duration;
use sge::{dbm_yt, load, migrate};
use tokio_postgres::{Client, NoTls};

use crate::init::PgInstance;

/**
What only a database shows:
1. a snapshot's data: each PostgreSQL type as its CSV has it, NULL and not
2. the yt queue: the cluster's sg_migration row, the pending rows the driver reads, marked
3. a user that is not there yet: refused without createdb, created - and its database - with
4. Postgres's own error for a statement it refuses, and its messages in our language
*/

#[tokio::test]
async fn database_types_yt_queue_and_new_roles() -> Result<(), String> {
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
    // the database and its history table
    migrate(pg.with_src(&case0(), vec!["createdb", "yes"]), false, &init::LOG).await?;
    let mut c = client(pg, &pg.db_name).await?;
    snapshot_types(pg, &mut c).await?;
    yt_queue(pg, &mut c).await?;
    new_role(pg).await?;
    pg_errors(pg, &mut c).await
}

fn case0() -> String {
    "./tests/test_scen/case0".to_string()
}

fn err(e: tokio_postgres::Error) -> String {
    e.to_string()
}

async fn client(pg: &PgInstance, db: &str) -> Result<Client, String> {
    let (c, conn) = tokio_postgres::connect(&format!("host=localhost port={} user={} password={} dbname={}",
        pg.port, init::os_user(), pg.db_pwd, db), NoTls).await.map_err(err)?;
    tokio::spawn(conn);
    Ok(c)
}

/// [etl] each type's value as a snapshot's CSV has it: in a nullable column a NULL is
/// empty - NULL for text, and the text NULL escaped - and in a NOT NULL one the value
async fn snapshot_types(pg: &PgInstance, c: &mut Client) -> Result<(), String> {
    c.batch_execute("
        create table public.types (i2 smallint, i4 int, i8 bigint, f4 real, f8 double precision, b bool,
            d date, t time, ts timestamp, tz timestamptz, s text);
        insert into public.types values (2, 4, 8, 1.5, 2.25, true, '2026-10-20', '12:30:05',
            '2026-10-20 12:30:05', '2026-10-20 12:30:05+00', 'NULL');
        insert into public.types values (null, null, null, null, null, null, null, null, null, null, null);
        create table public.types_nn (i2 smallint not null, i8 bigint not null, f4 real not null,
            f8 double precision not null, b bool not null, d date not null, t time not null,
            ts timestamp not null, tz timestamptz not null, s text not null);
        insert into public.types_nn values (2, 8, 1.5, 2.25, false, '2026-10-20', '12:30:05',
            '2026-10-20 12:30:05', '2026-10-20 12:30:05+00', 'y');
    ").await.map_err(err)?;
    let dir = format!("{}/snap", pg.work_dir);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // schema-qualified, and found in whichever schema has it
    load(true, pg.with_src(&case0(), vec!["snapshot_data", "public.types,types_nn", "snapshot_to", &dir]), &init::LOG).await?;
    let csv = |f: &str| fs::read_to_string(format!("{}/{}", dir, f)).map_err(|e| format!("{}: {}", f, e));
    assert_eq!(csv("D1__public_types.csv")?, "i2,i4,i8,f4,f8,b,d,t,ts,tz,s\n\
        2,4,8,1.5,2.25,true,2026-10-20,12:30:05,2026-10-20 12:30:05,2026-10-20 12:30:05 UTC,\\NULL\n\
        ,,,,,,,,,,NULL\n");
    assert_eq!(csv("D1__public_types_nn.csv")?, "i2,i8,f4,f8,b,d,t,ts,tz,s\n\
        2,8,1.5,2.25,false,2026-10-20,12:30:05,2026-10-20 12:30:05,2026-10-20 12:30:05 UTC,y\n");
    assert!(csv(&format!("D1__{}.yaml", pg.db_name))?.contains("tableName: types_nn"));
    Ok(())
}

/// [yt] the cluster's sg_migration row, read by a yt run; the pending rows the driver
/// rolls, in the planner's order - a done or blank-target one is not - marked done or
/// failed, the reason cut to the column
async fn yt_queue(pg: &PgInstance, c: &mut Client) -> Result<(), String> {
    let hist = "public.sge_history";
    let cfg = pg.with_src(&case0(), vec!["yt_cluster", "true", "yt_config_table", "public.yt_config"]);
    assert_eq!(dbm_yt::sg_migration(c, &cfg).await, None, "no config table: SchemaGuard's defaults");
    c.batch_execute("
        create table public.yt_config (module char, name text, value text);
        insert into public.yt_config values ('C', 'sg_migration', 'with_size_cut');
        insert into public.sge_history (source, target, source_type, version, ok, msg) values
            ('V2__b.sql', 'n2', 'V', '2', false, 'yt:pending'),
            ('V2__b.sql', 'n3', 'V', '2', false, 'yt:pending'),
            ('V2__b.sql', null, 'V', '2', false, 'yt:pending'),
            ('V2__b.sql', 'n4', 'V', '2', true, 'yt:pending');
    ").await.map_err(err)?;
    assert_eq!(dbm_yt::sg_migration(c, &cfg).await.as_deref(), Some("with_size_cut"));
    migrate(cfg, false, &init::LOG).await?;

    let q = dbm_yt::pending(c, hist).await?;
    assert_eq!(q.iter().map(|p| p.target.as_str()).collect::<Vec<_>>(), ["n2", "n3"]);
    assert_eq!((q[0].source.as_str(), q[0].source_type.as_str(), q[0].version.as_str()), ("V2__b.sql", "V", "2"));
    dbm_yt::mark(c, hist, q[0].id, true, "", 3).await?;
    dbm_yt::mark(c, hist, q[1].id, false, &"x".repeat(300), 4).await?;
    let rows = c.query("select ok, msg, took_sec from public.sge_history where id = any($1) order by id",
        &[&vec![q[0].id, q[1].id]]).await.map_err(err)?;
    assert_eq!((rows[0].get::<_, bool>(0), rows[0].get::<_, String>(1), rows[0].get::<_, i64>(2)), (true, "yt:done".into(), 3));
    let failed: String = rows[1].get(1);
    assert!(!rows[1].get::<_, bool>(0));
    assert!(failed.starts_with("yt:failed xxx") && failed.chars().count() == 250, "{}", failed);
    assert!(dbm_yt::pending(c, hist).await?.is_empty(), "both marked: nothing left to roll");
    assert!(dbm_yt::pending(c, "public.no_such_table").await.is_err());
    assert!(dbm_yt::mark(c, "public.no_such_table", 1, true, "", 0).await.is_err());
    Ok(())
}

/// [role] a user that is not there: refused without createdb; with it, created with
/// its password - a quote in it - and owning the database created for it
async fn new_role(pg: &PgInstance) -> Result<(), String> {
    let mut cfg = pg.with_src(&case0(), vec!["db_name", "dbrole", "db_user", "sge_new_role", "db_password", "pw'1"]);
    let e = migrate(cfg.clone(), false, &init::LOG).await.unwrap_err();
    assert!(e.starts_with("SGE-E009 ") && e.contains("dbrole"), "{}", e);
    cfg.insert(&"createdb".to_string(), "yes");
    migrate(cfg, false, &init::LOG).await?;
    let c = client(pg, "dbrole").await?;
    let owner: String = c.query_one("select pg_get_userbyid(datdba)::text from pg_database where datname = 'dbrole'", &[])
        .await.map_err(err)?.get(0);
    assert_eq!(owner, "sge_new_role");
    let tables: i64 = c.query_one("select count(*) from pg_tables where tablename = 'sge_history'", &[])
        .await.map_err(err)?.get(0);
    assert_eq!(tables, 1, "migrated as the new user");
    Ok(())
}

async fn lc_messages(c: &Client) -> Result<String, String> {
    Ok(c.query_one("show lc_messages", &[]).await.map_err(err)?.get(0))
}

/// [pg] a statement Postgres refuses comes with its own words - SQLSTATE and message - not
/// tokio_postgres's "db error"; and the server is asked for its messages in our language
/// when the role may set lc_messages, and left alone, unharmed, when it may not
async fn pg_errors(pg: &PgInstance, c: &mut Client) -> Result<(), String> {
    let dir = format!("{}/refused", pg.work_dir);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    fs::write(format!("{}/S1__refused.yaml", dir), "database:\n  - schemaName: public\n    tables:\n\
        \x20     - table:\n          tableName: refused\n          columns:\n\
        \x20           - column: { name: id, type: nosuchtype }\n").map_err(|e| e.to_string())?;
    let e = migrate(pg.with_src(&dir, vec![]), false, &init::LOG).await.unwrap_err();
    assert!(e.starts_with("SGE-E701 ") && e.contains("42704") && e.contains("nosuchtype") && !e.contains("db error"), "{}", e);
    // the line of the statement the server pointed at, the ^ under the type
    let lines: Vec<&str> = e.lines().collect();
    let caret = lines.iter().position(|l| l.trim() == "^").unwrap_or_else(|| panic!("no ^ in {}", e));
    let col = lines[caret].chars().count() - 1;
    assert_eq!(lines[caret - 1].chars().skip(col).take(10).collect::<String>(), "nosuchtype", "{}", e);

    // the superuser: the locale, when the server has it - asked of the server first
    let lang = std::env::var("LANG").unwrap_or_default();
    let want = if lang.starts_with("en_") { lang.clone() } else { "en_US.UTF-8".to_string() };
    let before = lc_messages(c).await?;
    let server_has = c.batch_execute(&format!("SET lc_messages TO '{}'", want)).await.is_ok();
    c.batch_execute(&format!("SET lc_messages TO '{}'", before)).await.map_err(err)?;
    schema_guard_tokio::set_lc_messages(c, "en").await;
    assert_eq!(lc_messages(c).await?, if server_has { want } else { before.clone() });
    schema_guard_tokio::set_lc_messages(c, "xx").await;

    // a role that may not set it: as it was, and its connection still good
    c.batch_execute("create role sge_plain login").await.map_err(err)?;
    let (plain, conn) = tokio_postgres::connect(&format!("host=localhost port={} user=sge_plain dbname={}",
        pg.port, pg.db_name), NoTls).await.map_err(err)?;
    tokio::spawn(conn);
    let as_was = lc_messages(&plain).await?;
    schema_guard_tokio::set_lc_messages(&plain, "en").await;
    assert_eq!(lc_messages(&plain).await?, as_was);
    let one: i32 = plain.query_one("select 1", &[]).await.map_err(err)?.get(0);
    assert_eq!(one, 1);
    Ok(())
}
