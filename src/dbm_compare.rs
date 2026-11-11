//! [compare] Two databases' schemas compared table by table, column by column: what a data diff of them
//! needs to know first. diff_data (data-diff) compares the data only and leaves the schemas to sge: a
//! comprehensive diff, schema and data, is this compare, then diff_data's for the data.
//!
//! A table is the same as the target's ([TableCompare::Match]), the target's has every reference column of
//! its type and more ([TableCompare::TargetSuperset]), or it differs ([TableCompare::Mismatch]): the target
//! lacks the table, a reference column, or has a column of another type. A table named in `exclude` that
//! differs is [TableCompare::Excluded] instead - its data is still compared, row by row. The schemas are
//! schema_guard's (`load_info_schema`), as `snapshot` reads them; the messages are in sge's language.
//!
//! The data step is diff_data's ([compare_data]): the tables the schema compare lets through ([data_pairs])
//! compared row by row, as the SQL that makes the target's rows the reference's.

use std::collections::{BTreeMap, HashSet};

use diff_data::{DiffConfig, DiffReport, TablePair};
use schema_guard_tokio::loader::{load_info_schema, InfoSchemaType, PgTable};
use tokio_postgres::Transaction;

/// [compare] One table's verdict
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableCompare {
    /// the same columns, of the same types
    Match,
    /// every reference column, of its type, and these more (sorted)
    TargetSuperset { extra_columns: Vec<String> },
    /// differs, but `exclude` names it: its data is compared all the same
    Excluded,
    /// differs: why
    Mismatch { message: String },
}

/// [compare] One table's columns compared, the reference's against the target's
pub fn compare_table(reference: &PgTable, target: &PgTable, excluded: bool) -> TableCompare {
    let mut missing: Vec<&String> = reference.columns.keys().filter(|c| !target.columns.contains_key(*c)).collect();
    missing.sort();
    let mut retyped: Vec<(&String, &str, &str)> = reference
        .columns
        .iter()
        .filter_map(|(name, r)| {
            let t = target.columns.get(name)?;
            (r.column_type != t.column_type).then_some((name, r.column_type.as_str(), t.column_type.as_str()))
        })
        .collect();
    retyped.sort();

    let differs = if !missing.is_empty() {
        let columns = missing.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(", ");
        Some(tr!("compare.columns_missing", columns = columns))
    } else if !retyped.is_empty() {
        let columns = retyped
            .iter()
            .map(|(c, r, t)| tr!("compare.type_differs", column = c, reference = r, target = t))
            .collect::<Vec<_>>()
            .join("; ");
        Some(tr!("compare.types_differ", columns = columns))
    } else {
        None
    };
    if let Some(message) = differs {
        return if excluded { TableCompare::Excluded } else { TableCompare::Mismatch { message } };
    }

    let mut extra_columns: Vec<String> = target.columns.keys().filter(|c| !reference.columns.contains_key(*c)).cloned().collect();
    if extra_columns.is_empty() {
        return TableCompare::Match;
    }
    extra_columns.sort();
    TableCompare::TargetSuperset { extra_columns }
}

/// [compare] Every table of the reference schema compared with the target schema's of its name, by table
/// name. `exclude` names tables in lowercase.
pub fn compare_schemas(
    reference: &InfoSchemaType,
    target: &InfoSchemaType,
    ref_schema: &str,
    target_schema: &str,
    exclude: &HashSet<String>,
) -> BTreeMap<String, TableCompare> {
    let none = Default::default();
    let ref_tables = reference.get(ref_schema).unwrap_or(&none);
    let target_tables = target.get(target_schema).unwrap_or(&none);
    ref_tables
        .iter()
        .map(|(name, table)| {
            let excluded = exclude.contains(&name.to_lowercase());
            let verdict = match target_tables.get(name) {
                Some(t) => compare_table(table, t, excluded),
                None if excluded => TableCompare::Excluded,
                None => TableCompare::Mismatch { message: tr!("compare.table_missing", table = name) },
            };
            (name.clone(), verdict)
        })
        .collect()
}

/// [compare] A database's schemas, as compare_schemas takes them: schema_guard's, read in `db`
pub async fn load(db: &mut Transaction<'_>, db_name: &str) -> Result<InfoSchemaType, String> {
    load_info_schema(db_name, db).await
}

/// [compare] The tables whose data the data step compares, by the schema compare's verdicts: one that
/// matches, one the target has wider, and one `exclude` names (its data compared all the same, as the
/// module says). One that differs is left out: its rows cannot be written into the target's table as they
/// are. As diff_data takes them: `<schema>.<table>` on each side, in the verdicts' order (by name)
pub fn data_pairs(verdicts: &BTreeMap<String, TableCompare>, ref_schema: &str, target_schema: &str) -> Vec<TablePair> {
    verdicts
        .iter()
        .filter(|(_, v)| !matches!(v, TableCompare::Mismatch { .. }))
        .map(|(name, _)| TablePair { reference: format!("{}.{}", ref_schema, name), target: format!("{}.{}", target_schema, name) })
        .collect()
}

/// [compare] A comprehensive diff's data step: the tables [data_pairs] keeps, their data compared by
/// diff_data with `config` - its pools, output and options; its tables are these, whatever it held. No
/// table to compare (every one differs) runs nothing and reports none: diff_data with no table would
/// compare every table of its schema. diff_data's messages are in sge's language
pub async fn compare_data(
    mut config: DiffConfig,
    verdicts: &BTreeMap<String, TableCompare>,
    ref_schema: &str,
    target_schema: &str,
) -> Result<DiffReport, String> {
    config.tables = data_pairs(verdicts, ref_schema, target_schema);
    if config.tables.is_empty() {
        return Ok(DiffReport::default());
    }
    diff_data::i18n::set_lang(&crate::i18n::lang());
    diff_data::run_diff(config).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_guard_tokio::loader::PgColumnDfn;
    use std::collections::HashMap;
    use tokio_postgres::NoTls;

    fn table(cols: &[(&str, &str)]) -> PgTable {
        PgTable {
            table_name: "t".into(),
            columns: cols
                .iter()
                .enumerate()
                .map(|(i, (n, ty))| {
                    (n.to_string(), PgColumnDfn {
                        column_name: n.to_string(),
                        column_type: ty.to_string(),
                        column_default: None,
                        sql: None,
                        fk: None,
                        pk: i == 0,
                        nullable: true,
                        sort_order: i,
                        column_comment: None,
                    })
                })
                .collect(),
            fks: HashMap::new(),
            triggers: HashMap::new(),
            indexes: HashMap::new(),
            grants: HashMap::new(),
            primary_key: None,
            sort_order: 0,
            table_comment: None,
            owner: None,
        }
    }

    /// [compare] the same, more on the target, or a difference - unless the table is excluded
    #[test]
    fn a_target_table_matches_has_more_or_differs() {
        crate::i18n::set_lang("en");
        let base = table(&[("id", "int4"), ("name", "text")]);
        assert_eq!(compare_table(&base, &base, false), TableCompare::Match);
        let wider = table(&[("id", "int4"), ("name", "text"), ("z", "int8"), ("extra", "int8")]);
        assert_eq!(compare_table(&base, &wider, false), TableCompare::TargetSuperset { extra_columns: vec!["extra".into(), "z".into()] });

        let narrower = table(&[("id", "int4")]);
        let wide_ref = table(&[("id", "int4"), ("b", "text"), ("a", "text")]);
        assert_eq!(compare_table(&wide_ref, &narrower, false),
            TableCompare::Mismatch { message: "reference columns missing in the target: a, b".into() });
        assert_eq!(compare_table(&base, &narrower, true), TableCompare::Excluded);

        let retyped = table(&[("id", "int8"), ("name", "varchar")]);
        assert_eq!(compare_table(&base, &retyped, false), TableCompare::Mismatch {
            message: "columns of another type: id: int4 in the reference, int8 in the target; \
                name: text in the reference, varchar in the target".into(),
        });
        assert_eq!(compare_table(&base, &retyped, true), TableCompare::Excluded);
    }

    /// [compare] a schema by its tables' names: one the target lacks differs, unless excluded
    #[test]
    fn every_reference_table_is_compared_with_the_targets_of_its_name() {
        crate::i18n::set_lang("en");
        let schema = |tables: Vec<(&str, PgTable)>| -> InfoSchemaType {
            let mut s = InfoSchemaType::new();
            s.insert("public".into(), tables.into_iter().map(|(n, t)| (n.to_string(), t)).collect());
            s
        };
        let reference = schema(vec![("a", table(&[("id", "int4")])), ("only_ref", table(&[("id", "int4")]))]);
        let target = schema(vec![("a", table(&[("id", "int4")]))]);
        let found = compare_schemas(&reference, &target, "public", "public", &HashSet::new());
        assert_eq!(found["a"], TableCompare::Match);
        assert_eq!(found["only_ref"], TableCompare::Mismatch { message: "table only_ref is not in the target schema".into() });
        let excluded: HashSet<String> = std::iter::once("only_ref".to_string()).collect();
        assert_eq!(compare_schemas(&reference, &target, "public", "public", &excluded)["only_ref"], TableCompare::Excluded);
        assert!(compare_schemas(&reference, &target, "nope", "public", &HashSet::new()).is_empty());
    }

    /// [compare] two live databases' schemas, read as schema_guard reads them
    #[tokio::test]
    async fn two_databases_compare_by_their_catalogs() {
        let Some(pg) = crate::tests::Pg::start("compare") else { return };
        crate::i18n::set_lang("en");
        let url = |db: &str| format!("host={} port={} user=postgres dbname={}", pg.dir.display(), pg.port, db);
        let (admin, conn) = tokio_postgres::connect(&url("postgres"), NoTls).await.unwrap();
        tokio::spawn(conn);
        for db in ["cmp_ref", "cmp_tgt"] {
            admin.batch_execute(&format!("CREATE DATABASE {}", db)).await.unwrap();
        }
        let (mut r, conn) = tokio_postgres::connect(&url("cmp_ref"), NoTls).await.unwrap();
        tokio::spawn(conn);
        let (mut t, conn) = tokio_postgres::connect(&url("cmp_tgt"), NoTls).await.unwrap();
        tokio::spawn(conn);
        r.batch_execute("CREATE TABLE users (id int PRIMARY KEY, name text); CREATE TABLE same (id int PRIMARY KEY); \
            CREATE TABLE retyped (id int PRIMARY KEY); CREATE TABLE only_ref (id int)").await.unwrap();
        t.batch_execute("CREATE TABLE users (id int PRIMARY KEY, name text, extra int); CREATE TABLE same (id int PRIMARY KEY); \
            CREATE TABLE retyped (id bigint PRIMARY KEY)").await.unwrap();

        let mut tx = r.transaction().await.unwrap();
        let reference = load(&mut tx, "cmp_ref").await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = t.transaction().await.unwrap();
        let target = load(&mut tx, "cmp_tgt").await.unwrap();
        tx.commit().await.unwrap();

        let found = compare_schemas(&reference, &target, "public", "public", &HashSet::new());
        assert_eq!(found["users"], TableCompare::TargetSuperset { extra_columns: vec!["extra".into()] });
        assert_eq!(found["same"], TableCompare::Match);
        assert!(matches!(&found["retyped"], TableCompare::Mismatch { message } if message.starts_with("columns of another type: id: ")), "{:?}", found["retyped"]);
        assert!(matches!(&found["only_ref"], TableCompare::Mismatch { message } if message.contains("only_ref")));
    }

    /// [compare] the data step takes the tables that match, are wider on the target or are excluded; one
    /// that differs is left out
    #[test]
    fn the_data_step_compares_the_tables_the_schemas_let_through() {
        let verdicts: BTreeMap<String, TableCompare> = vec![
            ("users", TableCompare::TargetSuperset { extra_columns: vec!["extra".into()] }),
            ("same", TableCompare::Match),
            ("retyped", TableCompare::Mismatch { message: "m".into() }),
            ("logs", TableCompare::Excluded),
        ]
        .into_iter()
        .map(|(n, v)| (n.to_string(), v))
        .collect();
        let pairs: Vec<(String, String)> =
            data_pairs(&verdicts, "public", "stage").into_iter().map(|p| (p.reference, p.target)).collect();
        assert_eq!(pairs, vec![
            ("public.logs".to_string(), "stage.logs".to_string()),
            ("public.same".to_string(), "stage.same".to_string()),
            ("public.users".to_string(), "stage.users".to_string()),
        ]);
        let all_differ: BTreeMap<String, TableCompare> =
            std::iter::once(("t".to_string(), TableCompare::Mismatch { message: "m".into() })).collect();
        assert!(data_pairs(&all_differ, "public", "public").is_empty());
    }

    /// [compare] no table to compare: nothing runs - the pools are never asked for a connection - and the
    /// report is empty, never every table of the schema
    #[tokio::test]
    async fn a_data_step_with_no_table_runs_nothing() {
        // a pool of a database that is not there: it connects only when asked (no idle connections)
        let pool = || diff_data::create_pool("host=/nonexistent dbname=none");
        let dir = std::env::temp_dir().join(format!("sge_compare_none_{}", std::process::id()));
        let mut config = DiffConfig::new(pool().await.unwrap(), pool().await.unwrap(), dir.join("diff.sql"));
        config.reference_schema = Some("public".into());
        let all_differ: BTreeMap<String, TableCompare> =
            std::iter::once(("t".to_string(), TableCompare::Mismatch { message: "m".into() })).collect();
        let report = compare_data(config, &all_differ, "public", "public").await.unwrap();
        assert!(report.tables.is_empty() && report.warnings.is_empty() && report.master_script.is_none());
        assert!(!dir.exists(), "no file written");
    }

    /// [compare] schema, then data, on two live databases: the target's missing and changed rows of the
    /// tables whose schemas let them through, as SQL; a retyped table's data left alone. diff_data's
    /// messages in sge's language
    #[tokio::test]
    async fn a_comprehensive_diff_compares_the_schemas_then_the_data() {
        let Some(pg) = crate::tests::Pg::start("compare_data") else { return };
        crate::i18n::set_lang("en");
        assert_eq!(diff_data::i18n::lang(), "en", "sge's language is diff_data's");
        let url = |db: &str| format!("host={} port={} user=postgres dbname={}", pg.dir.display(), pg.port, db);
        let (admin, conn) = tokio_postgres::connect(&url("postgres"), NoTls).await.unwrap();
        tokio::spawn(conn);
        for db in ["dd_ref", "dd_tgt"] {
            admin.batch_execute(&format!("CREATE DATABASE {}", db)).await.unwrap();
        }
        let (mut r, conn) = tokio_postgres::connect(&url("dd_ref"), NoTls).await.unwrap();
        tokio::spawn(conn);
        let (mut t, conn) = tokio_postgres::connect(&url("dd_tgt"), NoTls).await.unwrap();
        tokio::spawn(conn);
        r.batch_execute("CREATE TABLE users (id int PRIMARY KEY, name text); INSERT INTO users VALUES (1, 'ann'), (2, 'bob'), (3, 'cy'); \
            CREATE TABLE retyped (id int PRIMARY KEY, v text); INSERT INTO retyped VALUES (1, 'x')").await.unwrap();
        t.batch_execute("CREATE TABLE users (id int PRIMARY KEY, name text, extra int); INSERT INTO users VALUES (1, 'ann', 7), (2, 'BOB', 7); \
            CREATE TABLE retyped (id bigint PRIMARY KEY, v text)").await.unwrap();

        let mut tx = r.transaction().await.unwrap();
        let reference = load(&mut tx, "dd_ref").await.unwrap();
        tx.commit().await.unwrap();
        let mut tx = t.transaction().await.unwrap();
        let target = load(&mut tx, "dd_tgt").await.unwrap();
        tx.commit().await.unwrap();
        let verdicts = compare_schemas(&reference, &target, "public", "public", &HashSet::new());
        assert!(matches!(verdicts["retyped"], TableCompare::Mismatch { .. }));

        let dir = std::env::temp_dir().join(format!("sge_compare_data_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sql = dir.join("diff.sql");
        let mut config = DiffConfig::new(
            diff_data::create_pool(&url("dd_ref")).await.unwrap(),
            diff_data::create_pool(&url("dd_tgt")).await.unwrap(),
            sql.clone(),
        );
        config.single_file = true;
        config.show_progress = false;
        let report = compare_data(config, &verdicts, "public", "public").await.unwrap();
        let tables: Vec<&str> = report.tables.iter().map(|t| t.table_name.as_str()).collect();
        assert!(tables.iter().all(|t| t.ends_with("users")), "the retyped table is not compared: {:?}", tables);
        let users = report.tables.iter().find(|t| t.table_name.ends_with("users")).expect("users compared");
        assert_eq!((users.inserts, users.updates), (1, 1), "{:?}", users);
        let text = std::fs::read_to_string(&sql).unwrap();
        assert!(text.contains("'cy'") && text.contains("'bob'"), "{}", text);
        assert!(!text.to_lowercase().contains("retyped"), "{}", text);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
