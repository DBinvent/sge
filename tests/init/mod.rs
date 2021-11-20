#![allow(unused, warnings)]

extern crate rdbm;

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
use rdbm::dbm_cfg::ConfigMap;

pub const PG_VER: &str = "12";

lazy_static! {
    pub static ref LOG: Logger = logger(true);
    // pub static ref  DBM: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

    pub static ref DB: Arc<RwLock<bool>> = {
        println!("===> Starting DB...");
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

/*
  echo " 2: working dir for postgres DB (like 'data')"
  echo " 3: port to bind and appending working folder name (like 5434)"
  echo " 4: database name to create (default: $USER)"
  echo " 5: optional version of postgres (default 12)"

*/
///work_dir: &str, port: u16, db_name: &str,
pub fn prepare_db(dir: &str, user: &str) -> Result<PgInstance, String> {
    if let Ok(ev) = env::var("RDBM_IT") {
        if ev.as_str() == "ignore" {
           return Err(format!("migration_tests RDBM_IT set to: {}", ev));
        }
    }
    let ver = env::var("PG_VER")
        .unwrap_or(PG_VER.to_string())
        .parse::<u8>()
        .map_err(|e| format!("Parsing env PG_VER for testing: {} (default is {})", e, PG_VER))
        .unwrap();

    // let work_dir = uuid();
    let work_dir = format!("{}/dd{}", dir, uuid());

    let port = pseudo_rnd(5555, 7777);
    let db_name = format!("db{}", uuid());
    let db_pwd = uuid();

    fs::create_dir_all(work_dir.as_str());

    println!("Starting DB on ./postgres.sh start {} {} {} {} {} ... ", work_dir, port, ver, db_name, db_pwd);

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
                    println!("{}", e);
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
    cfg.insert(&"createdb_user".to_string(), env::var("USER").unwrap().as_str());
    cfg.insert(&"createdb_pwd".to_string(), db_pwd.as_str());
    cfg.insert(&"db_name".to_string(), db_name.as_str());
    cfg.insert(&"db_user".to_string(), env::var("USER").unwrap().as_str());
    cfg.insert(&"db_password".to_string(), db_pwd.as_str());
    cfg.insert(&"db_port".to_string(), port.to_string().as_str());
    cfg.insert(&"db_host".to_string(), "localhost");

    Ok(PgInstance{work_dir, port, db_name, db_pwd, ver, cfg})
}

pub fn cleanup_db(pg: PgInstance) {
    println!("\n\nCleanup on {}", pg.work_dir);
    match Command::new("./postgres.sh")
        .current_dir("./tests")
        .arg("stop")
        .arg(pg.work_dir.clone())
        .arg(pg.port.to_string())
        .output().map_err(|e| format!("{}", e)) {
        Ok(pc) => {
            println!("stop: [{}] {} {}", pc.status.success(), String::from_utf8_lossy(&pc.stderr), String::from_utf8_lossy(&pc.stdout));
        }
        Err(e) => {
            println!("stop: {}", e);
        }
    }

}

/// pg_dump -s
pub fn dump_db(pg: &PgInstance) -> Result<String, String> {
    println!("Dumping:  pg_dump -s -h localhost -p {} {}", pg.port, pg.db_name);
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


pub fn compare_with(d: &String, dump: &str, b: String) -> Result<(), String> {
    let u = env::var("USER").unwrap();
    let a = fs::read_to_string(format!("{}/{}", d, dump).as_str())
            .map_err(|e| format!("reading [{}/{}] {}", d, dump, e))?;
    fs::write(format!("{}/{}.out", d, dump), &b); // dump the dump
    let a:Vec<String> = a.split("\n").map(|l| l.replace("$USER", u.as_str())).collect();
    let b:Vec<&str> = b.split("\n").collect();
    let mut i = 0usize;
    loop {
        let x = if a.len() == 0 {""} else { a[min(a.len() - 1, i)].as_str() };
        let y = if b.len() == 0 {""} else { b[min(b.len() - 1, i)] };
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
    println!("DIFF at {} {} vs .dump", d, dump);
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