use lazy_static::lazy_static;

use chrono::{DateTime, NaiveDateTime};
use openssl::ssl::{SslConnector, SslMethod};
use postgres::{Client, NoTls, Transaction};
use postgres_openssl::MakeTlsConnector;
use rdbm::dbm_yml::ConfigMap;
use slog::Logger;
use std::fs;
use std::time::SystemTime;

pub fn license(cfg: ConfigMap, log: &Logger) -> Result<(), String> {
    let email = cfg.get("email", "");
    let sbs = cfg.get("subscribe", "");
    let lic = cfg.get("license_file", "");

    info!(log, "L email {}", email);
    Ok(())
}

pub fn is_valid(cfg: &ConfigMap, log: &Logger) -> Result<bool, String> {
    match cfg.get_opt("") {
        None => {}
        Some(_) => {}
    }
    Ok(true)
}

struct License {
    issued_to: String, //"Home"
    email: String,     //"v.krinitsyn@gmail.com"
    notice: String,    //"это поле свободно редактируется"
    connects: Vec<Connection>,
    api_key_valid_until: String, //"2021-12-12"
    api_key: String,             //"<jwt token 300-500char>"
    license: String,             //"<signature_see_bellow -- это будет строчка 300-500 символов>"
    key: String,                 //"<this client  RSA private key 40 строк х64char>"
}

struct Connection {
    valid_until: NaiveDateTime, //"2021-12-12"
    hosts: Vec<String>,
    db_names: Vec<String>,
}

impl License {
    fn load(file: &str) -> Result<Self, String> {
        unimplemented!()
    }

    fn validate(&self) -> Result<(), String> {
        unimplemented!()
    }
}
