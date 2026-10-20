#![allow(unused, warnings)]

extern crate sge;

use lazy_static::lazy_static;
use std::sync::{RwLock, Arc};
use std::{thread, fs, env};
use slog::Logger;
use sloggers::terminal::{TerminalLoggerBuilder, Destination};
use sloggers::types::{Format, Severity};
use sloggers::Build;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH, Duration};
use uuid::Uuid;
use std::cmp::min;
use std::process::{Command, Output};
use std::io::{Read, Error};
use sge::dbm_cfg::ConfigMap;

/// PG_VER's last resort, when no initdb is found where newest_installed() looks
pub const PG_VER: &str = "12";

/// [pg] The newest PostgreSQL with an initdb installed - Debian's
/// /usr/lib/postgresql/N, RHEL's /usr/pgsql-N: PG_VER's default, so the tests run
/// where 12 is long gone. postgres.sh finds that version's initdb under /usr.
pub fn newest_installed() -> Option<String> {
    let mut best: Option<u32> = None;
    for (dir, prefix) in [("/usr/lib/postgresql", ""), ("/usr", "pgsql-")].iter() {
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if let Some(v) = name.strip_prefix(prefix).and_then(|v| v.parse::<u32>().ok()) {
                    if e.path().join("bin/initdb").exists() && best.map_or(true, |b| v > b) {
                        best = Some(v);
                    }
                }
            }
        }
    }
    best.map(|v| v.to_string())
}

/// [it] A database the tests could not prepare: skipped when SGE_IT=ignore asks for
/// it, a failure otherwise - ignoring it left a machine with no PostgreSQL green, with
/// nothing tested
pub fn prepare_failed(what: &str, e: &str) {
    if env::var("SGE_IT").map_or(false, |v| v == "ignore") {
        error!(LOG, "SGE_IT=ignore: skipped, {} not prepared: {}", what, e);
    } else {
        panic!("{} not prepared: {} - are the PostgreSQL server binaries installed? \
            SGE_IT=ignore skips the integration tests", what, e);
    }
}

/// [user] The OS user, whom the throwaway cluster's superuser is named after (initdb's
/// default): $USER, else $LOGNAME, else `id -un` - a container often sets neither,
/// and unwrapping $USER panicked there
pub fn os_user() -> String {
    env::var("USER").or_else(|_| env::var("LOGNAME")).unwrap_or_else(|_| {
        Command::new("id").arg("-un").output().ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "postgres".to_string())
    })
}

lazy_static! {
    pub static ref LOG: Logger = logger(true);
    // pub static ref  DBM: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

    pub static ref DB: Arc<RwLock<bool>> = {
        info!(LOG, "===> Starting DB...");
        thread::spawn(move || {
//
        });

        Arc::new(RwLock::new(true))
    };

}

pub(crate) fn logger(debug: bool) -> Logger {
    let mut builder = TerminalLoggerBuilder::new();
    builder.channel_size(1024);
    builder.destination(Destination::Stdout);
    builder.format(Format::Compact);
    builder.level(if debug {
        Severity::Debug
    } else {
        Severity::Info
    });
    builder.build().unwrap()
}

#[derive(Debug, Clone)]
pub struct PgInstance {
    pub work_dir: String,
    pub port: u16,
    pub db_name: String,
    pub db_pwd: String,
    pub ver: u8,
    pub cfg: ConfigMap,
}

impl PgInstance {
    pub fn with_src(&self, d: &String, opts: Vec<&str>) -> ConfigMap {
        let mut cfg = self.cfg.clone();
        cfg.insert( & "source_dir".to_string(), d.as_str());
        if opts.len() > 1 {
            for i in 0..opts.len()/2{
                cfg.insert(&opts[i*2].to_string(), opts[i*2 + 1]);
            }
        }
        cfg
    }
}

const TESTS_DIR: &str = "./tests";
#[cfg(not(target_os = "linux"))]
const TMP_TEST_DIR: &str = "./tmp_tests";
#[cfg(target_os = "linux")]
const TMP_TEST_DIR: &str = "/tmp/tests";

/*
  echo " 2: working dir for postgres DB (like 'data')"
  echo " 3: port to bind and appending working folder name (like 5434)"
  echo " 4: database name to create (default: $USER)"
  echo " 5: optional version of postgres (default 12)"

*/
///work_dir: &str, port: u16, db_name: &str,
pub fn prepare_db() -> Result<PgInstance, String> {
    if let Ok(ev) = env::var("SGE_IT") {
        if ev.as_str() == "ignore" {
           return Err(format!("migration_tests SGE_IT set to: {}", ev));
        }
    }
    let ver = env::var("PG_VER")
        .unwrap_or_else(|_| newest_installed().unwrap_or(PG_VER.to_string()))
        .parse::<u8>()
        .map_err(|e| format!("Parsing env PG_VER for testing: {} (default is {})", e, PG_VER))
        .unwrap();

    // let work_dir = uuid();
    let work_dir = format!("{}/dd{}", TMP_TEST_DIR, uuid());

    let port = pseudo_rnd(5555, 7777);
    let db_name = format!("db{}", uuid());
    let db_pwd = uuid();

    fs::create_dir_all(work_dir.as_str());

    info!(LOG, "Starting DB on ./postgres.sh start {} {} {} {} {} ... ", work_dir, port, ver, db_name, db_pwd);

    let mut pc = Command::new("./postgres.sh");
        pc.current_dir(TESTS_DIR)
        .arg("start")
        .arg(work_dir.clone())
        .arg(port.to_string())
        .arg(ver.to_string())
        .arg(db_pwd.clone());
    let status : Arc<RwLock<Result<String, String>>> = Arc::new(RwLock::new(Ok("".to_string())));
    let sc = status.clone();
    thread::spawn(move || {
        match pc.output() {
            Ok(p) => {
                if p.status.success() {
                    let mut writer = sc.write().unwrap();
                    *writer = Ok(format!("postgres: {}", String::from_utf8_lossy(&p.stdout)));
                } else {
                    let mut writer = sc.write().unwrap();
                    *writer = Err(format!("starting postgres: {}", String::from_utf8_lossy(&p.stdout)));
                }
            }
            Err(e) => {
                let mut writer = sc.write().unwrap();
                *writer = Err(format!("postgres start: {}", e));
            }
        }
    });

    for i in 0..10 {
        thread::sleep(Duration::from_secs(1));
        let res = status.read().unwrap();
        match &*res {
            Ok(e) => {
                if e.len() > 0 {
                    info!(LOG, "{}", e);
                    break;
                } else {
                    print!(".");
                }
            }
            Err(e) => {
                return Err(e.clone());
            }
        }
    }

    let mut cfg = ConfigMap::load("")?;
    cfg.insert(&"createdb_user".to_string(), os_user().as_str());
    cfg.insert(&"createdb_pwd".to_string(), db_pwd.as_str());
    cfg.insert(&"db_name".to_string(), db_name.as_str());
    cfg.insert(&"db_user".to_string(), os_user().as_str());
    cfg.insert(&"db_password".to_string(), db_pwd.as_str());
    cfg.insert(&"db_port".to_string(), port.to_string().as_str());
    cfg.insert(&"db_host".to_string(), "localhost");

    Ok(PgInstance{work_dir, port, db_name, db_pwd, ver, cfg})
}

pub fn cleanup_db(pg: PgInstance) {
    info!(LOG, "\n\nCleanup on {}", pg.work_dir);
    match Command::new("./postgres.sh")
        .current_dir("./tests")
        .arg("stop")
        .arg(pg.work_dir.clone())
        .arg(pg.port.to_string())
        .output().map_err(|e| format!("{}", e)) {
        Ok(pc) => {
            info!(LOG, "stop: [{}] {} {}", pc.status.success(), String::from_utf8_lossy(&pc.stderr), String::from_utf8_lossy(&pc.stdout));
        }
        Err(e) => {
            info!(LOG, "stop: {}", e);
        }
    }

}

/// pg_dump -s
pub fn dump_db(pg: &PgInstance) -> Result<String, String> {
    info!(LOG, "Dumping:  pg_dump -s -h localhost -p {} {}", pg.port, pg.db_name);
    let p = Command::new("pg_dump").arg("-s")
        .arg("-h").arg("localhost")
        .arg("-p").arg(pg.port.to_string().as_str())
        .arg(pg.db_name.as_str()).output().map_err(|e| format!("{}", e))?;
    if p.status.success() {
        let dbd = String::from_utf8_lossy(&p.stdout).split("\n")
            .filter(|l| !(l.len()==0 || l.starts_with("--")
            || l.starts_with("SET")
            || l.starts_with("SELECT pg_catalog.set_config(")
            || l.starts_with("COMMENT ON")
            // PostgreSQL 18 wraps dumps in \restrict/\unrestrict psql
            // meta-commands, and the restrict token is RANDOM per dump - so a
            // golden file can never match one. Filtered like the other
            // non-schema preamble above; this is a dump format change, not a
            // schema difference.
            || l.starts_with("\\")
            ))
            .fold(String::new(), |mut data, line| {
						data.push_str(&line);
						data.push('\n');
						data
					});
        Ok(dbd)
    } else {
        Err(format!("pg_dump: {} {}", String::from_utf8_lossy(&p.stderr), String::from_utf8_lossy(&p.stdout)))
    }
}

pub const ALTER_SCHEMA: &str = "ALTER SCHEMA";
pub const OWNER_TO: &str = "OWNER TO";
pub const REVOKE_USAGE: &str = "REVOKE USAGE ON SCHEMA";
pub const GRANT_ALL : &str = "GRANT ALL ON SCHEMA ";

/// PostgreSQL 18's pg_dump writes `ALTER SEQUENCE x OWNER TO` where earlier
/// versions wrote `ALTER TABLE x OWNER TO`. The two are interchangeable -
/// ALTER TABLE has always accepted a sequence - so the keyword is NORMALISED
/// rather than the line filtered out: dropping it would stop the fixture
/// asserting that ownership is set at all, which is a real thing to check.
fn norm_dump_line(x: &str) -> String {
    if x.contains(OWNER_TO) && x.trim_start().starts_with("ALTER SEQUENCE ") {
        x.replacen("ALTER SEQUENCE ", "ALTER TABLE ", 1)
    } else {
        x.to_string()
    }
}

/// [pg15] PostgreSQL 15 gave `public` to `pg_database_owner` - the role that stands
/// for whoever owns the database: here the test's user, who owned `public` outright
/// in the PostgreSQL 12 the fixtures were written with. So it reads as that user.
fn norm_owner(x: &str, u: &str) -> String {
    x.replace("owner: pg_database_owner", format!("owner: {}", u).as_str())
}

fn fix_owner(x: &str) -> bool {
    (x.contains(ALTER_SCHEMA) && x.contains(OWNER_TO))
    || x.contains(REVOKE_USAGE)
    || x.contains(GRANT_ALL)
}

pub fn compare_with(d: &String, dump: &str, b: String) -> Result<(), String> {
    let u = os_user();
    let a = fs::read_to_string(format!("{}/{}", d, dump).as_str())
            .map_err(|e| format!("reading [{}/{}] {}", d, dump, e))?;
    fs::write(format!("{}/{}.out", d, dump), &b); // dump the dump
    let a:Vec<String> = a.split("\n")
        .map(|l| l.replace("$USER", u.as_str()))
        .filter(|x| !fix_owner(x.as_str()))
        .map(|l| norm_dump_line(l.as_str()))
        .collect();
    let b:Vec<String> = b.split("\n")
        .filter(|x| !fix_owner(*x))
        .map(|l| norm_dump_line(norm_owner(l, u.as_str()).as_str()))
        .collect();
    let mut i = 0usize;
    loop {
        let x = if a.len() == 0 {""} else { a[min(a.len() - 1, i)].trim() };
        let y = if b.len() == 0 {""} else { b[min(b.len() - 1, i)].trim() };
        if x != y
        {
            return Err(format!("not equals at line({}/{}) [{}/{}:{}]: \n[{}]\n[{}]", d, dump, i+1, a.len(), b.len(), x, y));
        }
        if a.len() <= i || b.len() <= i {
           return Ok(());// break;
        }
        i += 1;

    }
    Ok(())
}

/// diff with $1 $1.dump
pub fn diff(d: &String, dump: &str, ) -> Result<(), String> {
    info!(LOG, "DIFF at {} {} vs .dump", d, dump);
    let p = Command::new("diff")
        .arg(format!("{}/{}", d,  dump).as_str())
        .arg(format!("{}/{}.dump", d,  dump).as_str())
        .output().map_err(|e| format!("DIFF {}", e))?;
    if p.status.success() {
        Ok(())
    } else {
        Err(format!("diff: {} {}", String::from_utf8_lossy(&p.stderr), String::from_utf8_lossy(&p.stdout)))
    }
}


/// pseudo random function based on timer
#[inline]
fn pseudo_rnd(min: u16, max: u16) -> u16 {
	let pseudo_random = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap()
		.subsec_nanos();
    let pseudo_random = pseudo_random % (u16::MAX - min - 1) as u32;
	prc(min, max, min + pseudo_random as u16, 0)
}

#[inline]
fn prc(min: u16, max: u16, r: u16, i: u16) -> u16 {
	if i > 3 {
		min + 1
	} else {
		if r < min {
			if r + min < max {
				r + min
			} else {
				prc(min, max, r + min / 2, i + 1)
			}
		} else if r > max {
			prc(min, max, r % max, i + 1)
		} else if r == max {
			r - 1
		} else {
			r
		}
	}
}

//
#[inline]
fn uuid() -> String {
    let x = Uuid::new_v4();
    x.to_hyphenated().to_string()[..8].into()
}

#[cfg(test)]
#[allow(unused)]
mod tests {
    use std::thread::sleep;
    use std::time::Duration;

    use super::*;

    #[test]
    fn test_test() {

    }

}