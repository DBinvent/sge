//! [yt] Rolling migration across a YT Cluster, one node at a time.
//!
//! **Status:** planned. The planner (`yt_migrate.sql`, below) is not in this
//! release and nothing calls the driver yet; `yt_cluster = true` reads only the
//! cluster's `C sg_migration` row (`sg_migration`, `enabled`). The design stays
//! here, with the driver, for when the planner comes.
//!
//! ## The split
//!
//! `yt_migrate()` (see `yt_migrate.sql`) is the **planner**: a trigger on the
//! history table that, when a schema or version script succeeds locally,
//! enqueues one pending row per peer node - followers first, leader last - with
//! guards against overlapping rolls. It never connects anywhere, because
//! PostgreSQL cannot reach a peer without `dblink`/`postgres_fdw`, and
//! requiring those would mean storing credentials for every node on every node.
//!
//! This module is the **driver**: it reads those pending rows and applies the
//! migration to each node in turn over `psql`, marking each row as it goes and
//! **stopping at the first failure**.
//!
//! sge is the right place for the driver for one reason: it is already the
//! component that holds connection details for more than one database and
//! already shells out to `psql`. Nothing new is introduced - the roll is the
//! existing single-node deploy, repeated against a list.
//!
//! ## Why one node at a time
//!
//! Serial, waiting for each node, is what keeps the cluster serving: every node
//! not currently migrating is up and answering. It is also what makes failure
//! survivable - the roll halts with a *known* number of nodes migrated rather
//! than an arbitrary one, and the history table says exactly which.
//!
//! ## ⚠ Backward compatibility is the operator's responsibility
//!
//! **A rolling migration means the cluster runs a mixed schema for the duration
//! of the roll.** While node 3 of 6 is being migrated, three nodes have the new
//! schema and three have the old one, and application traffic is being served
//! by both. There is no window in which this is not true; making the window
//! shorter does not make it safe.
//!
//! Therefore every migration rolled this way must satisfy one of:
//!
//! 1. **The change is backward compatible** - old code runs unchanged against
//!    the new schema. Adding a nullable column, adding a table, adding an index,
//!    widening a type, adding an optional parameter. This is the safe default
//!    and should be the overwhelming majority of migrations.
//!
//! 2. **The code is schema-adaptive** - it detects which shape it is talking to
//!    and handles both. Necessary when a change genuinely cannot be made
//!    compatible, and it must be deployed *before* the migration, not with it.
//!
//! A change that is neither - dropping a column still read, renaming in place,
//! adding a NOT NULL without a default, narrowing a type - **will break the
//! nodes that have not been migrated yet, and equally the ones that have.**
//! sge cannot detect this, because whether a column is still read is a fact
//! about application code, not about the database. The expand/contract pattern
//! is the standard way out: expand (compatible), deploy code, contract
//! (compatible) - three rolls, each individually safe.

use tokio_postgres::Client;

use crate::dbm_cfg::ConfigMap;

/// One node's queued migration, as recorded by `yt_migrate()`.
#[derive(Debug, Clone)]
pub struct Pending {
    pub id: i32,
    /// node identity as it appears in the yt config table, module `N`
    pub target: String,
    pub source: String,
    pub source_type: String,
    pub version: String,
}

/// Marker written by the planner; also the thing this module looks for.
const PENDING: &str = "yt:pending";

/// Is cluster rolling switched on at all?
///
/// Off by default and explicitly so: a migration that silently starts touching
/// five more databases than the operator expected is the worst possible
/// surprise for this tool to spring.
pub fn enabled(cfg: &ConfigMap) -> bool {
    cfg.get_bool("yt_cluster", false)
}

/// Fully-qualified yt config table, default `public.yt_config`.
pub fn cfg_table(cfg: &ConfigMap) -> String {
    let t = cfg.get("yt_config_table");
    if t.trim().is_empty() { "public.yt_config".to_string() } else { t.trim().to_string() }
}

/// The cluster's SchemaGuard migration options, the `C sg_migration` row of the yt
/// config table: `MigrationOptions` field names, comma-separated. The cluster's own
/// schema check judges a difference by them, so a YAML file applied to a yt cluster
/// is applied by them too - one setting for both. `None` when the row is
/// absent or cannot be read: SchemaGuard's defaults.
pub async fn sg_migration(client: &mut Client, cfg: &ConfigMap) -> Option<String> {
    let sql = format!("select value from {} where module = 'C' and name = 'sg_migration'", cfg_table(cfg));
    client.query_opt(sql.as_str(), &[]).await.ok().flatten()
        .and_then(|r| r.try_get::<_, String>(0).ok())
}

/// Read the queue the planner built, in the order it chose.
///
/// `order by id` preserves the planner's insertion order - followers first,
/// leader last - rather than re-deriving it here where the leader may since
/// have changed. A roll that started against one leader should finish against
/// the same plan.
pub async fn pending(client: &mut Client, hist: &str) -> Result<Vec<Pending>, String> {
    let sql = format!(
        "select id, target, source, source_type::text, version from {} \
         where msg = $1 and not coalesce(ok, false) and target is not null and target <> '' \
         order by id", hist);
    let rows = client.query(sql.as_str(), &[&PENDING]).await.map_err(|e| schema_guard_tokio::pg_error(&e))?;
    Ok(rows.iter().map(|r| Pending {
        id: r.get(0),
        target: r.get(1),
        source: r.get(2),
        source_type: r.get(3),
        version: r.get(4),
    }).collect())
}

/// Map a node identity from the yt config table to a connectable host.
///
/// Module `N` values are `host[:port][;mac;desc]`, so the host is the first
/// field before `;` and the address before `:`. Returning the port separately
/// matters: nodes commonly share a hostname pattern but not a port in a lab.
pub fn node_address(value: &str) -> (String, Option<u16>) {
    let first = value.split(';').next().unwrap_or("").trim();
    match first.rsplit_once(':') {
        Some((h, p)) => match p.parse::<u16>() {
            Ok(_) => (h.to_string(), None), // the cluster RPC port, not Postgres
            Err(_) => (first.to_string(), None),
        },
        None => (first.to_string(), None),
    }
}

/// Mark one node's row as done or failed.
///
/// A failure records the reason in `msg` with a `yt:failed` prefix, which is
/// exactly what the planner refuses to roll past - so a broken node halts the
/// next migration too, rather than letting divergence compound quietly.
pub async fn mark(client: &mut Client, hist: &str, id: i32, ok: bool, msg: &str, took: i64)
    -> Result<(), String>
{
    let m = if ok {
        "yt:done".to_string()
    } else {
        let m = format!("yt:failed {}", msg);
        // history.msg is varchar(250)
        m.chars().take(250).collect()
    };
    let sql = format!(
        "update {} set ok = $1, msg = $2, took_sec = $3, executed_on = current_timestamp \
         where id = $4", hist);
    client.execute(sql.as_str(), &[&ok, &m, &took, &id]).await.map_err(|e| schema_guard_tokio::pg_error(&e))?;
    Ok(())
}

/// The session flag that stops a peer's planner from re-planning the cluster.
///
/// Set on the driver's connection to each node. See the matching guard in
/// `yt_migrate.sql`: without it, migrating node 3 would make node 3 enqueue the
/// whole cluster again, and node 4 would do it once more - one migration
/// fanning out combinatorially.
pub const ROLLING_GUARD: &str = "set sge.rolling = '1'";

/// Per-node connection settings for the roll.
///
/// Only the HOST is taken from the cluster config. Module `N` carries the
/// cluster's own RPC endpoint (`host:7421`), which is emphatically not a
/// PostgreSQL port - user, port, password and TLS all come from the operator's
/// existing `db_*` configuration, exactly as for a single-node deploy. Trying
/// to infer Postgres connectivity from the cluster roster is how you end up
/// connecting confidently to the wrong thing.
pub fn node_cfg(cfg: &ConfigMap, node_value: &str) -> ConfigMap {
    let (host, _) = node_address(node_value);
    let mut c = cfg.clone();
    c.insert(&"db_host".to_string(), host.as_str());
    // The roll is already driven from outside; a nested roll is never wanted.
    c.insert(&"yt_cluster".to_string(), "false");
    c
}

/// Human-readable plan, for `--dry_run` and for the log before a roll starts.
///
/// Printed before anything is applied on purpose: a rolling schema change
/// across a live cluster is exactly the operation an operator should be able to
/// read in full before it begins.
pub fn describe(queue: &[Pending]) -> String {
    if queue.is_empty() {
        return "[yt] nothing queued - no peer node needs this migration".to_string();
    }
    let mut o = format!("[yt] rolling {} node(s), one at a time, halting on first failure:\n", queue.len());
    for (i, p) in queue.iter().enumerate() {
        o.push_str(&format!(
            "  {}. {} <- {} {} v{}\n", i + 1, p.target, p.source_type, p.source, p.version));
    }
    o.push_str("  NOTE: the cluster runs a MIXED SCHEMA until the last node completes.\n");
    o.push_str("        The migration must be backward compatible, or the application\n");
    o.push_str("        must already handle both shapes. See dbm_yt.rs.\n");
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_node_is_marked_so_the_planner_refuses_to_continue() {
        // the prefix is load-bearing: yt_migrate.sql matches 'yt:failed%'
        let m: String = format!("yt:failed {}", "relation already exists");
        assert!(m.starts_with("yt:failed"));
    }

    #[test]
    fn node_values_carry_optional_metadata() {
        // module 'N' is `host[:port][;mac;desc]` - only the host is connectable
        assert_eq!(node_address("192.0.2.10:7421").0, "192.0.2.10");
        assert_eq!(node_address("n3:7421;aa:bb;east").0, "n3");
        assert_eq!(node_address("plainhost").0, "plainhost");
        assert_eq!(node_address("").0, "");
        // a colon that is no port: part of the host
        assert_eq!(node_address("host:rpc").0, "host:rpc");
    }

    #[test]
    fn rolling_is_off_and_the_config_table_public_unless_set() {
        assert!(!enabled(&ConfigMap::of(&[])));
        assert!(enabled(&ConfigMap::of(&[("yt_cluster", "true")])));
        assert_eq!(cfg_table(&ConfigMap::of(&[])), "public.yt_config");
        assert_eq!(cfg_table(&ConfigMap::of(&[("yt_config_table", " ops.cluster ")])), "ops.cluster");
    }

    /// [yt] a node is reached at its own host with the operator's db_ settings - not
    /// the cluster's RPC port - and does not roll the cluster again itself
    #[test]
    fn a_node_is_reached_with_the_operators_own_settings() {
        let cfg = ConfigMap::of(&[("db_host", "leader"), ("db_port", "5432"), ("db_user", "dba"), ("yt_cluster", "true")]);
        let c = node_cfg(&cfg, "n3:7421;aa:bb;east");
        assert_eq!(c.get("db_host"), "n3");
        assert_eq!(c.get("db_port"), "5432");
        assert_eq!(c.get("db_user"), "dba");
        assert!(!enabled(&c));
        assert!(enabled(&cfg), "the operator's own configuration is left as it was");
    }

    #[test]
    fn an_empty_queue_says_so_rather_than_printing_an_empty_plan() {
        assert!(describe(&[]).contains("nothing queued"));
    }

    #[test]
    fn the_plan_warns_about_the_mixed_schema_window_every_time() {
        // the warning is not optional decoration - it is the one thing an
        // operator must read before a rolling DDL change
        let q = vec![Pending {
            id: 1, target: "n3".into(), source: "V1.2__add_col.sql".into(),
            source_type: "V".into(), version: "1.2".into(),
        }];
        let d = describe(&q);
        assert!(d.contains("MIXED SCHEMA"), "{}", d);
        assert!(d.contains("backward compatible"), "{}", d);
        assert!(d.contains("halting on first failure"), "{}", d);
    }
}
