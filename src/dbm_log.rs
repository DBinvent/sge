use std::result::Result;
use std::collections::HashMap;
use serde::Deserialize;

use lazy_static::lazy_static;
use std::fmt::Display;
use std::{fmt, fs};
use postgres::{Transaction, Client, Error, Row};
use std::collections::hash_map::RandomState;
use crate::dbm_yml::{ConfigDefMap, YTable, ConfigMap};
use postgres::types::Timestamp;
// use serde::private::ser::constrain;
use crate::BASELINE;
use slog::Logger;
use std::process::{Command, Output};
use std::io::{Read, Write};
use base64::DecodeError;
use crypto_hash::Algorithm;
use std::fs::File;
// use sha1::Sha1;

/// loaded from DB or stored to DB, see [STATEMENT_LOG_TBL] and [STATEMENT_LOG_TABLE_YML] for table defenition
#[derive(Debug, Clone)]
pub(crate) struct StatementLog {
	pub id: u64, //\": (ct:\"serial\", dfn: \"PK\", pk: Some(true), dsr: \"PK\"),
	pub source: String, //\": (ct:\"varchar(250)\", dfn: \"\", dsr: \"\"),
	pub database: String, //\": (ct:\"varchar(150)\", dfn: \"\", dsr: \"\"),
	pub schemas_regxp: String, //\": (ct:\"varchar(250)\", dfn: \"\", dsr: \"\"),
	pub source_type: char, //\": (ct:\"char\", dfn: \"\", dsr: \"S: Schema, V: Version, R: Re-runnable\"),
	pub version: String, //\": (ct:\"varchar(50)\", dfn: \"\", dsr: \"\"),
	pub created: Timestamp<u64>, //\": (ct:\"timestamp\", dfn: \"default current_timestamp()\", dsr: \"\"),
	pub checksum: String, //\": (ct:\"bigint\", dfn: \"\", dsr: \"\"),
	pub scheduled_on: Timestamp<u64>, //\": (ct:\"timestamp\", dfn: \"\", dsr: \"\"),
	pub script_ver: String, //\": (ct:\"varchar(150)\", dfn: \"\", dsr: \"\"),
	pub executed_on: Timestamp<u64>, //\": (ct:\"timestamp\", dfn: \"\", dsr: \"\"),
	pub executed_by: String, //\": (ct:\"varchar(50)\", dfn: \"\", dsr: \"\"),
	pub ok: bool, // dfn: \"not null default false\", dsr: \"Successful execution of the script\"),
	pub msg: String, //\": (ct:\"varchar(250)\", dfn: \"\", dsr: \"\"),
	pub applied_by: String, //\": (ct:\"bigint\", dfn: \"\", dsr: \"\"),
	pub applied_from: String, //\": (ct:\"bigint\", dfn: \"\", dsr: \"\"),
	pub applied_to: String, //\": (ct:\"bigint\", dfn: \"\", dsr: \"\"),
}

/// information schema types: schema, table, column
pub (crate) type LS_TYPE = HashMap<String, (bool, String)>;

impl StatementLog {
	/// select table for update -- row level lock
	#[inline]
	pub(crate) fn load_all( t: &mut Transaction,  schema: &String, table: &String, cfg: &ConfigMap) -> Result<LS_TYPE, String> {
	    let db  = cfg.get("db_name", "");
		let lock = format!("select version from {}.{} where database = '{}' and source_type='S' and source = '{}' for update", schema, table, db, BASELINE);
		match t.query(lock.as_str(), &[]) {
			Ok(r) => {
				let mut res = HashMap::new();
				let sql = format!("select source_type||version, ok, checksum from {}.{} where database = '{}' where ok", schema, table, db);
				match t.query(sql.as_str(), &[]) {
					Ok(v) => {
						for r in v {
							let key:String = r.get(0);
							let val:bool = r.get(1);
							let checksum:String = r.get(2);
							res.insert(key, (val, checksum));
						}
						Ok(res)
					}
					Err(e) => {
						Err(format!("trying select: {}", e))
					}
				}
			}
			Err(e) => {
				Err(format!("trying select to lock: {}", e))
			}
		}
	}
	/// create a default [StatementLog]
	pub(crate) fn new(cfg: &ConfigMap) -> Self {
		StatementLog{
			id: 0,
			source: "".to_string(),
			database: cfg.get("db_name", ""),
			schemas_regxp: "".to_string(),
			source_type: ' ',
			version: "".to_string(),
			created: Timestamp::PosInfinity,
			checksum: "".into(),
			scheduled_on: Timestamp::PosInfinity,
			script_ver: "".to_string(),
			executed_on: Timestamp::PosInfinity,
			executed_by: "".to_string(),
			ok: false,
			msg: "".to_string(),
			applied_by: "".to_string(),
			applied_from: "".to_string(),
			applied_to: "".to_string()
		}
	}

	/// create an instance [StatementLog]
	pub(crate) fn instance(&self, code: char, version: &String, file: &String) -> Self {
		let mut insta = self.clone();
		insta.source_type = code;
		insta.version = version.clone();
		insta.source = file.clone();
		insta.checksum = match fs::File::open(file) {
			Ok( mut file) => hash_it(&mut file),
			Err(e) => e.to_string()
		};
		insta
	}

	/// deploy a file by psql call and store results
	#[inline]
	pub(crate) fn deploy(&self, logs: &LS_TYPE, pgsrv: &str, cfg: &ConfigMap, //code: char, version: &String, file: &String,
		 t: &mut Transaction, log: &Logger
	) -> Result<(), String> {
		let key = format!("{}{}", self.source_type, self.version);
		if let Some((ok, checks)) = logs.get(&key) {
			if *ok {
				if self.checksum == *checks {
					return Ok(());
				} else if self.source_type == 'V' {
					if cfg.get_bool("version_checksum", false) {
						// checksum different
						return Err(format!("Version [{}] script file checksum deployment mismatch", self.version));
					} else {
						return Ok(());
					}
				}
			}
		};
		let mut cmd = Command::new("psql");
		if let Ok(pwd) = cfg.get_opt("db_password") {
			cmd.env("PGSERVICEFILE", pgsrv);
		}
		if let Ok(port) = cfg.get_opt("db_port") {
			cmd.arg(format!("-p{}", port));
		}
		match cmd.arg(format!("--dbname={}", cfg.get("db_name", "")))
			.arg("-X").arg("-1").arg("-f").arg(&self.source).output() {
			Ok(output) => {
				let msg = format!("{}{}", String::from_utf8_lossy(output.stderr.as_ref()),
					String::from_utf8_lossy(output.stdout.as_ref()));
				if output.status.success() {
					info!(log, "> OK < \n{}", msg);
					self.fsave_ok(t);
				} else {
					error!(log, "{}", msg);
					self.fsave_err(msg, t);
				}
			}
			Err(e) => {
				error!(log, "{}", e);
				self.fsave_err(e.to_string(), t);
			}
		}
		Ok(())
	}

	pub(crate) fn fsave_ok(&self, t: &mut Transaction)  {
		    // let db_name = cfg.get("db_name", "");
		unimplemented!()

	}

	pub(crate) fn fsave_err(&self, err: String, t: &mut Transaction)  {
		    // let db_name = cfg.get("db_name", "");
		unimplemented!()

	}

	/// safe deployed by YTable.deploy()
	#[inline]
	pub(crate) fn save_ok(db_name: &String, t: &mut Transaction) -> Result<(), String> {

		unimplemented!()
	}

	/// safe deployed by YTable.deploy()
	#[inline]
	pub(crate) fn save_err(db_name: &String, error: &String, t: &mut Transaction) -> Result<(), String> {

		unimplemented!()
	}
}

#[inline]
fn arg(cmd: &mut Command, prefix: &str, key: &str, cfg: &ConfigMap){
	if let Ok(v) = cfg.get_po(prefix, key) {
		cmd.arg(format!("{}={}", key, v));
	}
}

#[inline]
fn parg(prefix: &str, key: &str, cfg: &ConfigMap) -> String{
	match cfg.get_po(prefix, key) {
		Ok(v) => {
			format!("{}={}", key, v)
		}
		Err(_) => "".into()
	}
}


const BUFFER_SIZE: usize = 1024;


/// Compute digest value for given `Reader` and print it
/// On any error simply return without doing anything
fn hash_it<R: Read>(reader: &mut R) -> String {
	let mut hasher = crypto_hash::Hasher::new(Algorithm::MD5);
    let mut buffer = [0u8; BUFFER_SIZE];
    loop {
        let n = match reader.read(&mut buffer) {
            Ok(n) => n,
            Err(_) => return "".into(),
        };
        hasher.write(&buffer[..n]);
        if n == 0 || n < BUFFER_SIZE {
            break;
        }
    }
    base64::encode(hasher.finish())
}


