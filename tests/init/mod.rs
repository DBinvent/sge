#![allow(unused, warnings)]

extern crate rdbm;

use lazy_static::lazy_static;
use std::sync::{RwLock, Arc};
use std::{thread, fs};
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

/*
  echo " 2: working dir for postgres DB (like 'data')"
  echo " 3: port to bind and appending working folder name (like 5434)"
  echo " 4: database name to create (default: $USER)"
  echo " 5: optional version of postgres (default 12)"

*/
///work_dir: &str, port: u16, db_name: &str,
pub fn prepare_db(dir: &str, user: &str, ver: u8) -> Result<(String, u16, String, String), String> {
    // let work_dir = uuid();
    let work_dir = format!("{}/dd{}", dir, uuid());

    let port = pseudo_rnd(5555, 6666);
    let db_name = format!("db{}", uuid());
    let db_pwd = uuid();

    println!("Starting DB on {} {} {} {}... ", work_dir, port, db_name, ver);

    let mut pc = Command::new("./postgres.sh");
        pc.current_dir("./tests")
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
                    *writer = Err(format!("postgres: {}", String::from_utf8_lossy(&p.stdout)));
                }
            }
            Err(e) => {
                let mut writer = sc.write().unwrap();
                *writer = Err(format!("postgres start: {}", e));
            }
        }
    });
    thread::sleep(Duration::from_secs(3));
    let res = status.read().unwrap();
    match &*res {
        Ok(e) => {
            println!("{}", e);
        }
        Err(e) => {
            return Err(e.clone());
        }
    }
    Ok((work_dir, port, db_name, db_pwd))
}

pub fn cleanup_db(work_dir: String, port: u16, db_name: String, ver: u8) {
    println!("\n\nCleanup on {}", work_dir);
    match Command::new("./postgres.sh")
        .current_dir("./tests")
        .arg("stop")
        .arg(work_dir.clone())
        .arg(port.to_string())
        .output().map_err(|e| format!("{}", e)) {
        Ok(pc) => {
            println!("stop: [{}] {} {}", pc.status.success(), String::from_utf8_lossy(&pc.stderr), String::from_utf8_lossy(&pc.stdout));
        }
        Err(e) => {
            println!("stop: {}", e);
        }
    }

}

pub fn dump_db(work_dir: &String, port: u16, db_name: &String, ver: u8) -> Result<String, String> {
    println!("Dumping:  pg_dump -s -h localhost -p {} {}", port, db_name);
    let p = Command::new("pg_dump").arg("-s")
        .arg("-h").arg("localhost")
        .arg("-p").arg(port.to_string().as_str())
        .arg(db_name.as_str()).output().map_err(|e| format!("{}", e))?;
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


pub fn compare_with(d: String, b: String) -> Result<(), String> {
    let a = fs::read_to_string(format!("{}/test.result", d).as_str())
            .map_err(|e| format!("reading [{}/test.result] {}", d, e))?;
    fs::write(format!("{}/dump.out", d), &b); // dump the dump
    let a:Vec<&str> = a.split("\n").collect();
    let b:Vec<&str> = b.split("\n").collect();
    let mut i = 0usize;
    loop {
        let x = if a.len() == 0 {""} else { a[min(a.len() - 1, i)] };
        let y = if b.len() == 0 {""} else { b[min(b.len() - 1, i)] };
        if x != y
        {
            return Err(format!("not equals at line [{}/{}:{}]: \n[{}]\n[{}]", i+1, a.len(), b.len(), x, y));
        }
        if a.len() <= i || b.len() <= i {
           return Ok(());// break;
        }
        i += 1;

    }
    Ok(())
}

/// pseudo random function based on timer
#[inline]
fn pseudo_rnd(min: u16, max: u16) -> u16 {
	let pseudo_random = SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap()
		.subsec_nanos();
	prc(min, max, pseudo_random as u16, 0)
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
        if let Err(e)  = compare_with("./tests/test_scen/case0".into(), "".into()) {
            assert!(true, "{}", e);
        };
        if let Err(e)  = compare_with("./tests/test_scen/case0".into(),
"CREATE TABLE public.rdbm_history (source varchar(250)  , scheduled_on timestamp  , catalog varchar(80)  , executed_by varchar(80)  , msg varchar(250)  , target varchar(250)  , source_type char  , id serial  , script_tag varchar(80)  , version varchar(80)  , created_on timestamp  , checksum varchar(80)  , took_sec bigint  , executed_on timestamp  , ok bool   ) ; \n\
\n\
COMMENT ON COLUMN public.rdbm_history.source_type IS 'The schema deployment log';\n\
COMMENT ON COLUMN public.rdbm_history.ok IS 'The schema deployment log';\n\
\n\
\n\
COMMENT ON TABLE public.rdbm_history IS 'The schema deployment log'; \n\
\n\
".into()) {
            assert!(false, "{}", e);
        }

    }

}