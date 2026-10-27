//! [superset] `--snapshot_format=superset`: the snapshot as Apache Superset's import bundle
//! (schema_guard_tokio's superset module, Superset 2.1 and later) - one dataset for each
//! table the snapshot read. Superset connects to the database by `--superset_uri`, or by
//! the db_* settings sge connected with; `--superset_uuid` names a connection Superset has
//! already, which the import keeps as it is.
use schema_guard_tokio::superset::SupersetDatabase;

use crate::dbm_cfg::{url_value, ConfigMap};

/// The connection of the snapshot's database, as the bundle names it; `user`, the role
/// sge's connection is
pub(crate) fn database(cfg: &ConfigMap, user: &str) -> Result<SupersetDatabase, String> {
    let name = cfg.get("db_name");
    let uri = match cfg.get_opt("superset_uri").filter(|u| !u.trim().is_empty()) {
        Some(uri) => uri,
        None => sqlalchemy_uri(cfg, user)?,
    };
    match cfg.get_opt("superset_uuid").filter(|u| !u.trim().is_empty()) {
        Some(uuid) => SupersetDatabase::existing(&name, &uri, &uuid),
        None => Ok(SupersetDatabase::new(&name, &uri)),
    }
}

/// The db_* settings given - as `connect` reads them, no default added - as an SQLAlchemy
/// URI: `postgresql://user:password@host:port/name`. The user is db_user, else `user`, the
/// role sge connected as (the OS user, as libpq takes it): left out, Superset logged in as
/// its own OS user, and the password went with it (review of 2026-10-26). A unix socket's
/// directory, which no URI host can be, as the `host` parameter, as psycopg2 takes it;
/// `sslmode` kept. The name is written as it is: SQLAlchemy 1.4 - Superset 2.1 to 4 -
/// decodes no database name, so `my%20db` was the name it asked for; one with a `?` in it
/// would end where the parameters begin, and is refused
pub(crate) fn sqlalchemy_uri(cfg: &ConfigMap, user: &str) -> Result<String, String> {
    let given = |k: &str| cfg.get_opt(k).filter(|v| !v.is_empty());
    let name = cfg.get("db_name");
    if name.contains('?') {
        return Err(tr!("etl.superset_name", name = name));
    }
    let mut uri = "postgresql://".to_string();
    if let Some(user) = given("db_user").or_else(|| Some(user.to_string()).filter(|u| !u.is_empty())) {
        uri.push_str(&url_value(&user));
        if let Some(password) = given("db_password") {
            uri.push(':');
            uri.push_str(&url_value(&password));
        }
        uri.push('@');
    }
    let mut query: Vec<String> = Vec::new();
    match given("db_host") {
        Some(socket) if socket.starts_with('/') => {
            query.push(format!("host={}", url_value(&socket)));
            if let Some(port) = given("db_port") {
                query.push(format!("port={}", url_value(&port)));
            }
        }
        host => {
            if let Some(host) = host {
                // an IPv6 address takes its brackets
                uri.push_str(&if host.contains(':') && !host.starts_with('[') { format!("[{}]", host) } else { host });
            }
            if let Some(port) = given("db_port") {
                uri.push(':');
                uri.push_str(&port);
            }
        }
    }
    uri.push('/');
    uri.push_str(&name);
    if let Some(mode) = given("db_sslmode") {
        query.push(format!("sslmode={}", url_value(&mode)));
    }
    if !query.is_empty() {
        uri.push('?');
        uri.push_str(&query.join("&"));
    }
    Ok(uri)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [superset] the db_* settings sge connected with, as the URI Superset connects by:
    /// a password with URL characters encoded, a unix socket as `host=`, an IPv6 address
    /// in brackets, sslmode kept; no db_user: the role sge connected as, with the password;
    /// the name as it is, and one with a `?` refused
    #[test]
    fn the_connection_settings_are_an_sqlalchemy_uri() {
        let uri = |pairs: &[(&str, &str)], user: &str| sqlalchemy_uri(&ConfigMap::of(pairs), user);
        assert_eq!(uri(&[("db_user", "app"), ("db_password", "p@s:w/d"), ("db_host", "db.local"), ("db_port", "5433"),
            ("db_name", "orders")], "vk").unwrap(), "postgresql://app:p%40s%3Aw%2Fd@db.local:5433/orders");
        assert_eq!(uri(&[("db_user", "app"), ("db_host", "/var/run/postgresql"), ("db_port", "5433"), ("db_name", "orders")], "vk")
            .unwrap(), "postgresql://app@/orders?host=%2Fvar%2Frun%2Fpostgresql&port=5433");
        assert_eq!(uri(&[("db_host", "::1"), ("db_name", "orders"), ("db_sslmode", "require")], "").unwrap(),
            "postgresql://[::1]/orders?sslmode=require");
        assert_eq!(uri(&[("db_host", "db1"), ("db_password", "pw"), ("db_name", "app")], "vk").unwrap(),
            "postgresql://vk:pw@db1/app", "no db_user: the role sge connected as, and the password kept");
        assert_eq!(uri(&[("db_name", "my db")], "").unwrap(), "postgresql:///my db", "as SQLAlchemy 1.4 reads it");
        assert!(uri(&[("db_name", "a?b")], "").unwrap_err().starts_with("SGE-E419 "));
    }

    /// [superset] `--superset_uri` over the settings; `--superset_uuid` names a connection
    /// Superset has already, checked and lower-cased - one that is no UUID, an error
    #[test]
    fn the_connection_is_the_one_given() {
        let cfg = ConfigMap::of(&[("db_name", "orders"), ("db_user", "app"), ("db_password", "s3cret"),
            ("superset_uri", "postgresql://bi:pw@warehouse/orders")]);
        let db = database(&cfg, "vk").unwrap();
        assert_eq!(db.database_name, "orders");
        assert_eq!(db.sqlalchemy_uri, "postgresql://bi:pw@warehouse/orders");
        let cfg = ConfigMap::of(&[("db_name", "orders"), ("superset_uuid", "6B9C2D1E-0000-4000-8000-00000000ABCD")]);
        assert_eq!(database(&cfg, "vk").unwrap().uuid(), "6b9c2d1e-0000-4000-8000-00000000abcd");
        let cfg = ConfigMap::of(&[("db_name", "orders"), ("superset_uuid", "nope")]);
        assert!(database(&cfg, "vk").unwrap_err().starts_with("SG-E073 "));
    }
}
