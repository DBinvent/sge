#[allow(unused)]
use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::{env, fs};

use crate::ConfigMap;
use chrono::{Duration, DateTime, Utc, NaiveDate, Datelike};
use crypto_hash::Algorithm;
use rand::thread_rng;
use rsa::{BigUint, PaddingScheme, PublicKey, PublicKeyParts, RSAPrivateKey, RSAPublicKey};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use slog::Logger;
use std::collections::HashMap;
use yaml_rust::{Yaml, YamlLoader};

const PK: &str = "tYV/+g8q0/K0DhWxcrTesII0/ludUrOxDsHTopTcV5FmyXzCrK75z3ARiK67pHa5r5sQCkstK0JKs2/izmp2sjfsm/lLSmLvViBgbzd96bQYZe6Z3zBhyLHa4xVNe5o+VeWX7tOI5EenG0JPkvfMGijS4AHbQs33SZ1z8UKRg8ES4Ab/YK3d/ddfJ8Fs+fvDyuoO+NK10te/wdsvZgsK87v36hLoZi7ZwKSkdzH0dxC6JqM/YYyA72fMFtoRw6s3gT8rdjuLHPuyIh7OC5W7ufH9Fe0r10tS0KeUsNo77ZM82HdknWbSsznl3RyWkj5C0dY9ez+TK3xSot4Gfj/a5tFUyLIJMP+QlPTQZlXO+04ClOJDvJBfd8A+x0i5DqQgbwV3rBSzAi30086pOTxmlZvfT7Zze1Q6KJZhCukNcLDLxqnDm8gV/i7/i3f5EmsIbOvKKBHdNi1QDDqmgG787BXxW9a34Po9APxT4nALBWPqLTMs1cx6MpOxJBJelKr9AQAB";

const INV_LL: &str =
    "Please check https://rdbm.dbinvent.com/license See './rdbm help license' for more details.";

#[allow(dead_code)]
const HOST: &str = "https://rdbm.dbinvent.com/";
const ABOUT_EXPIRE: u8 = 20; // days

/// on license action
pub fn license(cfg: &ConfigMap, log: &Logger, test: Option<&dyn Fn(&ApiRequest) -> String>) -> Result<(), String> {
    let now = cfg.now();
    let email = cfg.get("email");
    let dry_run = cfg.get_bool("dry_run", false);
    let license_file = cfg.get("license_file");
    let mut subsr: BTreeSet<String> = BTreeSet::new();
    cfg.get("subscribe").split(",").for_each(|s| {
        subsr.insert(s.into());
    });
    let db_host = cfg.get("db_host");
    let logdb_host = cfg.get("logdb_host");
    let db_name = cfg.get("db_name");
    let logdb_name = cfg.get("logdb_name");

    let do_subscribe = subsr.contains("*")
        || (db_host.len() > 0 && subsr.contains(&db_host))
        || (logdb_host.len() > 0 && subsr.contains(&logdb_host))
        || (db_name.len() > 0 && subsr.contains(&db_name))
        || (logdb_name.len() > 0 && subsr.contains(&logdb_name));

    if Path::new(license_file.as_str()).is_file() {
        match License::load(license_file.as_str()) {
            Ok(l) => match l.verify() {
                Ok(_) => {
                    info!(
                        log,
                        "License [{}] issued to: {} {}{}",
                        license_file,
                        l.issued_to,
                        l.email,
                        if do_subscribe {
                            ", will try to subscribe if necessary"
                        } else {
                            ""
                        }
                    );

                    let found = match l.find(&db_host, &logdb_host, &db_name, &logdb_name) {
                        None => false,
                        Some(until) => (until - now).num_days() >= 0
                    };

                    if found {
                        info!(log, "Good to use for your connection(s)! Thank you! ");
                        if l.is_about_expire(&now, ABOUT_EXPIRE) {
                            ApiRequest::Pull(l).call(&license_file, &now, dry_run, log, test)
                        } else {
                            Ok(())
                        }
                    } else if email.len() > 0 && do_subscribe {
                        let lc = CreateUpdateRequest::new(&email, &db_host, &logdb_host, &db_name, &logdb_name)?;
                        ApiRequest::Update(l, lc).call(&license_file, &now, dry_run, log, test)
                    } else {
                        Err(format!("The license loaded from file [{}] is not valid: {}. Set '--email' and use '--subscribe' to request", license_file, INV_LL))
                    }
                }
                Err(e) => Err(format!(
                    "The license loaded from file [{}] is not valid: {}. {}",
                    license_file, e, INV_LL
                )),
            },
            Err(e) => Err(format!(
                "The license loaded from file [{}] is not valid: {}. {}",
                license_file, e, INV_LL
            )),
        }
    } else if email.len() > 0 && do_subscribe {
        let lc = CreateUpdateRequest::new(&email, &db_host, &logdb_host, &db_name, &logdb_name)?;
        ApiRequest::Create(lc).call(&license_file, &now, dry_run, log, test)
    } else {
        Err(format!("No '--email' nor '--license_file' set, also might be helpful: --subscribe=* and db connections to set. Please check ./rdbm help license for more details."))
    }
}

pub enum ApiRequest {
    Create(CreateUpdateRequest),
    Update(License, CreateUpdateRequest),
    Pull(License),
}

impl ApiRequest {
    #[inline]
    fn method(&self) -> &str {
        match &self {
                ApiRequest::Create(_) => "creating",
                ApiRequest::Pull(_) => "checking",
                ApiRequest::Update(_,_) => "updating",
            }
    }

    #[inline]
    fn call(&self, file: &String, now: &LicenseDate, dry_run: bool, log: &Logger, test: Option<&dyn Fn(&ApiRequest) -> String>
    ) -> Result<(), String> {
        let url = format!("{}/api/license", api_host());
        let method = &self.method();
        if dry_run {
            info!(log, "About to call {} to {} license, but --dry_run set.", url, method);
            return Ok(());
        }
        let auth_header = "X-Header";
        let mut encm = HashMap::new();
        let response = match test {
            None => {
                let response = match &self {
                    ApiRequest::Create(lc) => {
                        let request = serde_json::to_string(lc)
                            .map_err(|e| format!("creating payload of a license create request: {}", e))?;
                        ureq::post(url.as_str()) //.set(auth_header, "newbie")
                            .send_string(request.as_str())
                            .map_err(|e| format!("{} license: {}", method, e))?
                    }
                    ApiRequest::Update(l, lu) => {
                        let _ = l.clean(&mut encm)?;
                        let request = serde_json::to_string(lu)
                            .map_err(|e| format!("creating payload of a license update request: {}", e))?;
                        ureq::put(url.as_str())
                            .set(auth_header, l.header()?.as_str())
                            .send_string(request.as_str())
                            .map_err(|e| format!("{} license: {}", method, e))?
                    }
                    ApiRequest::Pull(l) => {
                        let _ = l.clean(&mut encm)?;
                        ureq::get(format!("{}/{}", url.as_str(), l.api_key).as_str())
                            .set(auth_header, l.header()?.as_str())
                            .call()
                            .map_err(|e| format!("{} license: {}", method, e))?
                    }
                };
                debug!(log, "response [{}] from {} at {}", response.status(), method, url);
                response.into_string().map_err(|e| format!("getting response: {}", e))?
            }
            Some(callback) => callback(&self),
        };

        match self {
            ApiRequest::Create(_l) => {
                let response: ApiResponse = serde_json::from_str(response.as_str())
                    .map_err(|e| format!("converting response on create: {} from {}", e, response))?;
                if response.success {
                    println!("{}", response.result);
                    Ok(())
                } else {
                    Err(response.as_err())
                }
            }
            ApiRequest::Update(l, _) => LicenseResponse::save(file, response, l, encm, now, log),
            ApiRequest::Pull(l) => LicenseResponse::save(file, response, l, encm, now, log),
        }
    }
}

impl LicenseResponse {
    #[inline]
    fn save(file: &String, response: String, license: &License,
        encoded_map: HashMap<String, String>, now: &LicenseDate,
        log: &Logger,
    ) -> Result<(), String> {
        let lr: LicenseResponse = serde_json::from_str(response.as_str())
            .map_err(|e| format!("converting response on save: {} from {}", e, response))?;
        let mut lr = lr.result;
        debug!(log, "loaded: {:?} ", lr);

        if lr.patch(license, &encoded_map, now) {
            lr.verify()?;
            debug!(log, "saving to {}", file);
            fs::write(file.clone(), serde_yaml::to_string(&lr)
                .map_err(|e| format!("creating license: {}", e))?)
                .map_err(|e| format!("writing to [{}] {}", file, e))?;
        }
        Ok(())
    }
}

#[cfg(debug_assertions)]
fn api_host() -> String {
    "http://localhost:8000/".to_string()
}

#[cfg(not(debug_assertions))]
#[inline]
fn api_host() -> String {
    HOST.to_string()
}

/// Request to register user and issue an license (by email)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreateUpdateRequest {
    pub personal: bool,
    pub email: String,
    pub hosts: Vec<String>,
    pub db_names: Vec<String>,
}

#[inline]
fn append_name(name: &String, ip:bool, list: &mut Vec<String>) {
    let user = env::var("USER").unwrap_or("".to_string());
    if name.len() > 0 && name.as_str() != "*"
        && ( (ip  && !is_ip(name) )
        || (!ip && name != &user && name != &"postgres".to_string()
         && name == &crate::dbm_yml::safe_sql(name.clone())) )
    {
        list.push(name.clone());
    }
}

impl CreateUpdateRequest {
    #[inline]
    #[allow(dead_code)]
    fn new(
        email: &String,
        db_host: &String,
        logdb_host: &String,
        db_name: &String,
        logdb_name: &String,
    ) -> Result<Self, String> {
        let mut hosts = Vec::new();
        append_name(db_host, true, &mut hosts);
        append_name(logdb_host, true, &mut hosts);
        let mut db_names = Vec::new();
        append_name(db_name, false, &mut db_names);
        append_name(logdb_name, false, &mut db_names);

        Ok(CreateUpdateRequest {
            personal: email.find("mail").unwrap_or(0) > 0,
            email: email.clone(),
            hosts,
            db_names,
        })
    }

    #[inline]
    #[allow(dead_code)]
    pub fn validate(&self) -> Option<String> {
        if self.email.len() < 2 {
            Some("invalid email".into())
        } else {
            match self.email.find("@") {
                None => Some("invalid email".into()),
                Some(i) => {
                    if i == 0 || i == self.email.len() - 1 {
                        Some("invalid email".into())
                    } else {
                        if let Some(m) = valid_name(&self.hosts) {
                            Some(format!("invalid hosts: {}", m))
                        } else if let Some(m) = valid_name(&self.db_names) {
                            Some(format!("invalid db names: {}", m))
                        } else {
                            None
                        }
                    }
                }
            }
        }
    }
}

#[inline]
#[allow(dead_code)]
fn valid_name(names: &Vec<String>) -> Option<String> {
    for n in names {
        if let Some(_) = n
            .trim()
            .chars()
            .position(|c| c == ' ' || c == ';' || c == '*')
        {
            return Some(n.into());
        }
    }
    None
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct LicenseResponse {
    result: License,
    success: bool,
    errors: Vec<String>,
    messages: Vec<String>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct ApiResponse {
    result: String,
    success: bool,
    errors: Vec<ApiMessage>,
    messages: Vec<ApiMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiMessage {
    pub code: i32,
    pub message: String
}

impl ApiResponse {
    fn as_err(&self) -> String {
        let mut res = String::new();
        for e in &self.errors {
            if res.len() > 0 {res.push_str(", ");}
            res.push_str(e.message.as_str());
        }
        for e in &self.messages {
            if res.len() > 0 {res.push_str(", ");}
            res.push_str(e.message.as_str());
        }
        res
    }
}

#[cfg(debug_assertions)]
/// use with: "FORCE_CHECK:any" and "TEST_DAYS:int"
pub fn is_ok(cfg: &ConfigMap, log: &Logger) -> Result<bool, String> {
    match cfg.get_opt("FORCE_CHECK") {
        None => Ok(true),
        Some(_) => Ok(do_check(cfg, log)? >= 0)
    }
}

#[cfg(not(debug_assertions))]
#[inline]
/// on migrate action
pub fn is_ok(cfg: &ConfigMap, log: &Logger) -> Result<bool, String> {
    Ok(do_check(cfg, log)? >= 0)
}

pub fn do_check(cfg: &ConfigMap, _log: &Logger) -> Result<i64, String> {
    match cfg.get_opt("license_file") {
        None => Err("--license_file is not set".into()),
        Some(lf) => {
            let l = License::load(lf.as_str())?;
            l.verify()?;
            Ok(match l.find(
                &cfg.get("db_host"),
                &cfg.get("logdb_host"),
                &cfg.get("db_name"),
                &cfg.get("logdb_name")
            ) {
                None => -1,
                Some(until) => (until - cfg.now()).num_days()
            })
        }
    }
}

pub type LicenseDate = chrono::NaiveDate; //DateTime<FixedOffset>;
pub type LicenseTime = chrono::NaiveDateTime; //DateTime<FixedOffset>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct License {
    pub issued_to: String, //"Evaluate"
    pub email: String,     //"@gmail.com"
    pub notice: String,    //"это поле свободно редактируется"
    pub connects: Vec<Connection>,
    #[serde(with = "rfc3339")]
    pub api_key_valid_until: LicenseTime, //"2021-12-12"
    pub api_key: String, // "<jwt token 300-500char>"
    /// package name
    pub license_type: String,
    pub license: String, //"<signature_see_bellow -- это будет строчка 300-500 символов>"
    #[serde(skip)]
    pub crc: String,     // debug
    /// will be generated by openssl if connections are not encrypted AND 'cpk' not set
    pub key: String, //"<this client  RSA private key 40 строк х64char>"
    #[serde(skip)]
    /// clients RSA Pub key: use to encode connections, if they are not
    pub cpk: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    #[serde(with = "local_date_format")]
    pub valid_until: LicenseDate,
    pub hosts: BTreeSet<String>,
    pub db_names: BTreeSet<String>,
}

impl Connection {
    #[allow(unused, dead_code)]
    #[inline]
    /// digest stings (prepare for encryption) and encrypt by public key separately
    pub fn new(input: LicenseDate) -> Self {
        Connection {
            valid_until: input,
            hosts: Default::default(),
            db_names: Default::default(),
        }
    }

    #[inline]
    /// digest stings (prepare for encryption) and encrypt by public key separately
    pub(crate) fn encrypt_all(&self, public_key: &RSAPublicKey) -> Result<Vec<Vec<u8>>, String> {
        let mut encoded = Vec::new();
        for d in &self.db_names {
            encoded.push(Connection::encrypt(d, public_key)?);
        }
        for h in &self.hosts {
            encoded.push(Connection::encrypt(h, public_key)?);
        }
        Ok(encoded)
    }

    #[inline]
    fn encrypt(input: &String, public_key: &RSAPublicKey) -> Result<Vec<u8>, String> {
        if input.len() > 24 && input.as_bytes()[0] == b'~' {
            Ok(base64::decode(&input[1..]).map_err(|e| e.to_string())?)
        } else {
            let mut hasher = crypto_hash::Hasher::new(Algorithm::SHA256);
            let _ = hasher.write(input.as_bytes());
            let _ = hasher.write(&public_key.e().to_bytes_be());
            let _ = hasher.write(&public_key.n().to_bytes_be());
            let d = hasher.finish();
            Ok(Sha1::digest(&d).to_vec())
        }
    }

    pub fn encode(input: &String, public_key: &RSAPublicKey) -> Result<String, String> {
        if input.len() > 24 && input.as_bytes()[0] == b'~' {
            Ok(input.clone())
        } else {
            let mut res = base64::encode(Connection::encrypt(input, &public_key)?);
            res.insert(0, '~');
            Ok(res)
        }
    }

}

impl License {
    #[allow(dead_code)]
    pub fn blank() -> Self {
        License {
            issued_to: "".to_string(),
            email: "".to_string(),
            notice: "".to_string(),
            connects: vec![],
            api_key_valid_until: Utc::now().naive_utc(),
            api_key: "".to_string(),
            license_type: "".to_string(),
            license: "".to_string(),
            crc: "".to_string(),
            key: "".to_string(),
            cpk: None,
        }
    }

    #[inline]
    pub fn pubk(&self) -> Result<RSAPublicKey, String> {
        Ok(match &self.cpk {
            None => str2rsa(&self.key)?.to_public_key(),
            Some(s) => str2pub(s)?,
        })
    }
    // License

    #[inline]
    pub fn digest(&self) -> Result<(Vec<u8>, String), String> {
        let public_key = self.pubk()?;

        let mut cc = format!("{}", self.cnt());
        crc::a(&mut cc, &public_key.e().to_bytes_be());
        crc::a(&mut cc, &public_key.n().to_bytes_be());

        let mut hasher = crypto_hash::Hasher::new(Algorithm::SHA512);
        let _ = hasher.write(self.issued_to.as_bytes());
        let _ = hasher.write(self.email.as_bytes());
        for c in &self.connects {
            let _ = hasher.write(&c.valid_until.year().to_be_bytes());
            let _ = hasher.write(&c.valid_until.num_days_from_ce().to_be_bytes());
            for e in &c.encrypt_all(&public_key)? {
                crc::a(&mut cc, &e);
                let _ = hasher.write(e.as_ref());
            }
        }
        let d = hasher.finish();
        crc::a(&mut cc, &d);
        let d = Sha1::digest(&d).to_vec();
        crc::a(&mut cc, &d);
        Ok((d, cc))
    }
    // License

    #[inline]
    pub fn load(file: &str) -> Result<Self, String> {
        match fs::read_to_string(file) {
            Ok(input) => License::new(input, file),
            Err(e) => Err(format!("wrong config {}: {}", file, e)),
        }
    }

    #[inline]
    pub(crate) fn new(input: String, file: &str) -> Result<Self, String> {
        match YamlLoader::load_from_str(input.as_str()) {
            Ok(y) => {
                if y.len() > 0 {
                    let y = &y[0];
                    let mut connects = Vec::new();
                    if let Some(cv) = y["connects"].as_vec() {
                        for c in cv {
                            connects.push(Connection {
                                valid_until: to_date(c, "valid_until")?,
                                hosts: to_list(c, "hosts"),
                                db_names: to_list(c, "db_names"),
                            });
                        }
                    }
                    Ok(License {
                        license_type: yaml_as_str(y, "license_type"),
                        issued_to: yaml_as_str(y, "issued_to"),
                        email: yaml_as_str(y, "email"),
                        notice: yaml_as_str(y, "notice"),
                        connects,
                        api_key_valid_until: to_time(y, "api_key_valid_until")?,
                        api_key: yaml_as_str(y, "api_key"),
                        license: yaml_as_str(y, "license"),
                        crc: yaml_as_str(y, "crc"),
                        key: yaml_as_str(y, "key"),
                        cpk: None,
                    })
                } else {
                    Err(format!("no config in {} ", file))
                }
            }
            Err(e) => Err(format!("wrong license {}: {}", file, e)),
        }
    }
    // License

    #[inline]
    /// count total and encrypt connections except: localhost, postgres and $USER set on user_filter
    pub(crate) fn cnt(&self) -> usize {
        let mut count = 0;
        for c in &self.connects {
            count += c.db_names.len() + c.hosts.len();
        }
        count
    }

    #[inline]
    /// validate license integrity
    fn verify(&self) -> Result<(), String> {
        match base64::decode(&self.license) {
            Ok(sign) => {
                let (d, crc) = self.digest()?;
                match hardcoded()?.verify(
                    PaddingScheme::new_pss::<Sha1, _>(thread_rng()),
                    &d,
                    &sign,
                ) {
                    Ok(_) => Ok(()),
                    Err(e) => Err(format!(
                        "license: {}, signed: {}, calculated: {}",
                        e, self.crc, crc
                    )),
                }
            }
            Err(e) => Err(format!("reading license: {}", e)),
        }
    }
    // License

    #[inline]
    #[allow(dead_code)]
    pub(crate) fn rewrite(&self, input: String, file: &str) -> Result<(), String> {
        let mut l = License::new(input, "https://rdbm.dbinvent.com/")?;
        l.key = self.key.clone();

        match serde_yaml::to_string(&self) {
            Ok(l) => match fs::write(file, l) {
                Ok(_) => Ok(()),
                Err(e) => Err(format!("writing to [{}] {}", file, e)),
            },
            Err(e) => Err(format!("writing to [{}] {}", file, e)),
        }
    }
    // License

    #[inline]
    fn clean(&self, enc: &mut HashMap<String, String>) -> Result<String, String> {
        let mut lic = self.clone();
        lic.cpk = None;
        lic.api_key = "".into();
        lic.key = "".into();
        lic.notice = "".into();
        let public_key = self.pubk()?;
        for c in &mut lic.connects {
            let hosts = c.hosts.to_owned();
            c.hosts = BTreeSet::new();
            for host in &hosts {
                let e = Connection::encode(host, &public_key)?;
                enc.insert(e.clone(), host.clone());
                c.hosts.insert(e);
            }
            let db_names = c.db_names.to_owned();
            c.db_names = BTreeSet::new();
            for db_name in &db_names {
                let e =  Connection::encode(db_name, &public_key)?;
                enc.insert(e.clone(), db_name.clone());
                c.db_names.insert(e);
            }
        }
        serde_yaml::to_string(&lic).map_err(|e| format!("creating payload of a license: {}", e))
    }
    // License

    /// opposite to clean
    #[inline]
    fn patch(&mut self, lic: &License, enc: &HashMap<String, String>, now: &LicenseDate) -> bool {
        self.notice = lic.notice.clone();
        self.key = lic.key.clone();
        let mut self_now = now.clone(); //Utc::now().into();
        let mut lic_now = now.clone();
        for c in &lic.connects {
            if c.valid_until > lic_now {
                lic_now = c.valid_until.clone();
            }
        }
        for c in &mut self.connects {
            if c.valid_until > self_now {
                self_now = c.valid_until.clone();
            }
            let hosts = c.hosts.to_owned();
            c.hosts.clear();
            for host in &hosts {
                c.hosts.insert(enc.get(host).unwrap_or(host).into());
            }
            let db_names = c.db_names.to_owned();
            c.db_names.clear();
            for db_name in &db_names {
                c.db_names
                    .insert(enc.get(db_name).unwrap_or(db_name).into());
            }
        }
        lic_now > self_now
    }
    // License

    #[inline]
    fn find(&self, host_a: &String, host_b: &String, db_a: &String, db_b: &String) -> Option<LicenseDate> {
        let user = &env::var("USER").unwrap_or("".into());
        let pg = "postgres".to_string();
        let is_ip_a = is_ip(host_a);
        let is_ip_b = is_ip(host_b);
        // let mut far = Utc::today().naive_utc() - chrono::Duration::days(3000);
        let mut fnd: Option<LicenseDate> = None;
        let (db_a, db_b) = if db_a.len() == 0 && db_b.len() > 0 {
            (db_b, db_a)
        } else {
            (db_a, db_b)
        };
        for c in &self.connects {
            // if c.valid_until > far {
            //     far = c.valid_until.clone();
            // }
            if (c.hosts.contains("*")
                || ((is_ip_a || c.hosts.contains(host_a))
                    && (is_ip_b || c.hosts.contains(host_b))))
                && (((db_a == "" || db_a == user || db_a == &pg)
                     && (db_b == "" || db_b == user || db_b == &pg))
                    || c.db_names.contains("*")
                    || (c.db_names.contains(db_a)
                        && (db_b.len() == 0 || c.db_names.contains(db_b))))
            {
                if { if let Some(f) = &fnd {
                        c.valid_until > *f
                    } else { true }
                } {
                    fnd = Some(c.valid_until.clone());
                }
            }
        }

        fnd
    }
    // License

    /// generate authorization header
    #[inline]
    #[allow(dead_code)]
    pub fn header(&self) -> Result<String, String> {
        let now: LicenseTime = Utc::now().naive_utc();
        if self.api_key.len() < 30 {
            Err("api_key is not set".into())
        } else if self.api_key_valid_until < now {
            Err("api_key is outdated".into())
        } else if self.key.len() < 300 {
            Err("key is not set".into())
        } else {
            let rsa = match base64::decode(&self.key) {
                Ok(der_bytes) => match RSAPrivateKey::from_pkcs1(&der_bytes) {
                    Ok(key) => key,
                    Err(e) => {
                        return Err(format!("key is not good: {:?}", e));
                    }
                },
                Err(e) => {
                    return Err(format!("key is not good: {}", e));
                }
            };

            let d = self.header_digest();
            match rsa.sign_blinded(
                &mut thread_rng(),
                PaddingScheme::new_pss::<Sha1, _>(thread_rng()),
                &d,
            ) {
                Ok(s) => Ok(format!("{}:{}", self.api_key, base64::encode(s))),
                Err(e) => Err(format!("key is not good for sign: {}", e)),
            }
        }
    }
    // License

    #[inline]
    #[allow(dead_code)]
    fn header_digest(&self) -> Vec<u8> {
        let mut hasher = crypto_hash::Hasher::new(Algorithm::SHA256);
        let _ = hasher.write(self.api_key.as_bytes());
        let _ = hasher.write(self.email.as_bytes());
        Sha1::digest(&hasher.finish()).to_vec()
    }

    /// validate authorization header
    /// the auth must be clean of api-key (uuid)
    #[inline]
    #[allow(dead_code)]
    pub fn is_header(&self, auth: &str) -> Result<(), String> {
        match base64::decode(auth) {
            Ok(sign) => {
                let d = self.header_digest();
                match self.pubk()?.verify(
                    PaddingScheme::new_pss::<Sha1, _>(thread_rng()),
                    &d,
                    &sign,
                ) {
                    Ok(_) => Ok(()),
                    Err(e) => Err(format!("invalid auth header: {}", e)),
                }
            }
            Err(e) => Err(format!("reading header: {}", e)),
        }
    }
    // License

    #[inline]
    #[allow(dead_code)]
    fn is_about_expire(&self, now: &LicenseDate, pull_ahead_days: u8) -> bool {
        let about_expire: LicenseDate = (*now + Duration::days(pull_ahead_days as i64)).into();
        for c in &self.connects {
            if c.valid_until < about_expire {
                return true;
            }
        }
        false
    }

}

#[inline]
fn yaml_as_str(y: &Yaml, field: &str) -> String {
    y[field].as_str().unwrap_or("").into()
}

#[inline]
/// check for empty string, localhost, unix socket, or IP
fn is_ip(host: &String) -> bool {
    host.len() == 0
        || host.as_str() == "localhost"
        || host.as_bytes()[0] == b'/'
        || match ipaddress::IPAddress::parse(host) {
            Ok(_) => true,
            Err(_) => false,
        }
}

mod crc {
    use std::fmt::Write;

    #[cfg(not(debug_assertions))]
    #[inline]
    pub(crate) fn a(_s: &mut String, _d: &[u8]) {}

    #[cfg(debug_assertions)]
    #[inline]
    pub(crate) fn a(s: &mut String, d: &[u8]) {
        let _ = write!(
            s,
            ".{}",
            if d.len() == 0 {
                0
            } else if d.len() == 1 {
                d[0]
            } else {
                let mut r = d[0];
                for i in 1..d.len() {
                    r = r ^ d[i];
                }
                r
            }
        );
    }
}

#[inline]
fn to_list(yaml: &Yaml, input: &str) -> BTreeSet<String> {
    let mut r = BTreeSet::new();
    if let Some(list) = yaml[input].as_vec() {
        for l in list {
            if let Some(c) = l.as_str() {
                r.insert(c.into());
            }
        }
    }
    r
}

#[inline]
fn to_date(yaml: &Yaml, field: &str) -> Result<LicenseDate, String> {
    match yaml[field].as_str() {
        None => Err(format!("not date set in: {}", field)),
        Some(s) => NaiveDate::parse_from_str(s, DATE_FORMAT)
            .map_err(|e| format!("date parsing [{}] error: {}", s, e))
    }
}

#[inline]
fn to_time(yaml: &Yaml, field: &str) -> Result<LicenseTime, String> {
    match yaml[field].as_str() {
        None => Err(format!("not date set in: {}", field)),
        Some(s) => Ok(DateTime::parse_from_rfc3339(s)
            .map_err(|e| format!("date parsing [{}] error: {}", s, e))?.naive_utc())
    }
}

pub const DATE_FORMAT: &str = "%Y-%m-%d";

mod local_date_format {
    use chrono::NaiveDate;
    use serde::{self, Deserialize, Deserializer, Serializer};
    use crate::license::{LicenseDate, DATE_FORMAT};

    pub fn serialize<S>(date: &LicenseDate, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&date.format(DATE_FORMAT).to_string())
    }

    #[allow(dead_code)]
    pub fn deserialize<'de, D>(deserializer: D) -> Result<LicenseDate, D::Error>
    where
        D: Deserializer<'de>,
    {
        NaiveDate::parse_from_str(String::deserialize(deserializer)?.as_str(), DATE_FORMAT)
            .map_err(serde::de::Error::custom)
    }
}

mod rfc3339 {
    use chrono::{DateTime, Utc};
    use serde::{self, Deserialize, Deserializer, Serializer};
    use crate::license::LicenseTime;

    pub fn serialize<S>(date_time: &LicenseTime, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let s = DateTime::<Utc>::from_utc(*date_time, Utc).to_rfc3339();
        serializer.serialize_str(&s)
    }

    #[allow(dead_code)]
    pub fn deserialize<'de, D>(deserializer: D) -> Result<LicenseTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let dt = DateTime::parse_from_rfc3339(&s).map_err(serde::de::Error::custom)?;
        Ok(dt.naive_utc())
    }
}

/// create base64 string from RSA, use str2rsa(key).to_public_key() -> pub2str() -> str2pub()
#[inline]
#[allow(dead_code)]
pub fn pub2str(input: &RSAPublicKey) -> String {
    format!(
        "{}{}",
        base64::encode(input.n().to_bytes_be()),
        base64::encode(input.e().to_bytes_be()),
    )
}

#[inline]
pub fn str2pub(input: &String) -> Result<RSAPublicKey, String> {
    let i = input.len();
    if i < 100 {
        Err(format!("malformed for RSA key [{}]", input.len()))
    } else {
        let i = i - 4;
        match strs2pub(&input[..i], &input[i..]) {
            Ok(key) => Ok(key),
            Err(e) => Err(format!("{:?}", e)),
        }
    }
}

#[inline]
fn strs2pub(input_n: &str, input_e: &str) -> Result<RSAPublicKey, rsa::errors::Error> {
    match base64::decode(input_n) {
        Ok(bn) => match base64::decode(input_e) {
            Ok(be) => RSAPublicKey::new(
                BigUint::from_bytes_be(bn.as_ref()),
                BigUint::from_bytes_be(be.as_ref()),
            ),
            Err(e) => Err(rsa::errors::Error::ParseError {
                reason: format!("E: {}", e),
            }),
        },
        Err(e) => Err(rsa::errors::Error::ParseError {
            reason: format!("N: {}", e),
        }),
    }
}

#[inline]
pub fn str2rsa(input: &str) -> Result<RSAPrivateKey, String> {
    match base64::decode(input) {
        Ok(der_bytes) => {
            if der_bytes.len() > 1000 {
                match RSAPrivateKey::from_pkcs1(&der_bytes) {
                    Ok(key) => Ok(key),
                    Err(e) => Err(format!("{:?}", e)),
                }
            } else {
                Err(format!("file is too small for PKCS: {}", der_bytes.len()))
            }
        }
        Err(e) => Err(e.to_string()),
    }
}

#[inline]
/// for testing
pub(crate) fn hardcoded() -> Result<RSAPublicKey, String> {
    str2pub(&PK.into())
}

#[cfg(test)]
#[allow(unused)]
mod tests {
    use std::thread::sleep;
    use std::time::Duration;

    use super::*;
    use chrono::{Local, DateTime};

    #[test]
    fn test_date1() {
        let z = "-0510".to_string();

        match z[z.len() - 2..z.len()].parse::<i8>() {
            Ok(v) => assert_eq!(v, 10),
            Err(_) => assert!(false),
        };

        match z[..z.len() - 2].parse::<i8>() {
            Ok(v) => assert_eq!(v, -5),
            Err(_) => assert!(false),
        };
    }

    #[test]
    fn test_date() {
        let z = "-0500".to_string();
        match z[0..4].parse::<i8>() {
            Ok(v) => assert_eq!(v, -50i8),
            Err(_) => assert!(false),
        };
        let z = "0500".to_string();
        match z[0..if z.len() > 4 { 4 } else { 3 }].parse::<i8>() {
            Ok(v) => assert_eq!(v, 50i8),
            Err(_) => assert!(false),
        };

        let z = "+0500".to_string();
        match z[0..if z.len() > 4 { 4 } else { 3 }].parse::<i8>() {
            Ok(v) => assert_eq!(v, 50i8),
            Err(_) => assert!(false),
        };

        let one = Utc::now();
        sleep(Duration::from_millis(100));
        let two = Utc::now();
        // let t: DateTime<FixedOffset> = two.into();
        assert!(one < two);
        let mut ss: BTreeSet<String> = BTreeSet::new();
        "a".split(",").for_each(|c| {
            ss.insert(c.into());
        });
        assert_eq!(ss.len(), 1);
        assert_eq!(
            ss.iter().next().unwrap_or(&"".to_string()),
            &"a".to_string()
        );
    }

    #[test]
    fn test_ip() {
        assert!(is_ip(&"".into()));
        assert!(is_ip(&"127.0.0.1".into()));
        assert!(is_ip(&"localhost".into()));
        assert!(!is_ip(&"google.com".into()));
    }

    #[test]
    fn init_lic() -> Result<(), String> {
        let lf = "tests/signed_test_rdbm.license_ignore"; // TODO re-sign and unignore
        if Path::new(lf).is_file() {
            let l = License::load(lf)?;
            l.verify()?;
            let h = l.header()?;
            assert!(h.len() > 0);
            let i = h.find(":").unwrap();
            assert_eq!(l.api_key, h[..i]);
            l.is_header(&h[i + 1..])?;
        }
        Ok(())
    }

    #[test]
    fn clean_lic() -> Result<(), String> {
        let mut l = License::blank();
        let now: LicenseDate = Utc::today().naive_utc();
        l.cpk = Some(PK.into());
        l.key = "a".into();
        let mut hosts = BTreeSet::new();
        hosts.insert("h".to_string());
        let mut db_names = BTreeSet::new();
        db_names.insert("db".to_string());
        l.connects.push(Connection {
            valid_until: Utc::today().naive_utc(),
            hosts,
            db_names,
        });
        let mut enc = HashMap::new();
        let cl = l.clean(&mut enc)?;
        assert_eq!(enc.len(), 2);
        println!("{:?}", enc);

        let mut cl = License::new(cl, "test")?;
        assert_eq!(cl.connects.len(), 1);
        assert!(!cl.connects[0].hosts.contains("h"));
        assert!(!cl.connects[0].db_names.contains("db"));
        cl.patch(&l, &mut enc, &now);
        assert_eq!(cl.connects.len(), 1);
        assert!(cl.connects[0].hosts.contains("h"));
        assert!(cl.connects[0].db_names.contains("db"));
        Ok(())
    }

    #[test]
    fn test_append_name() {
        let user = env::var("USER").unwrap_or("".to_string());
        let mut v = Vec::new();
        append_name(&"".to_string(), true, &mut v);
        append_name(&"*".to_string(), true, &mut v);
        append_name(&"localhost".to_string(), true, &mut v);
        append_name(&"127.0.0.1".to_string(), true, &mut v);
        append_name(&"/var/".to_string(), true, &mut v);

        append_name(&"".to_string(), false, &mut v);
        append_name(&"*".to_string(), false, &mut v);
        append_name(&user, false, &mut v);
        assert_eq!(v.len(), 0);

        append_name(&"aaa".to_string(), true, &mut v);
        append_name(&"ooo".to_string(), false, &mut v);
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn test_find() {
        let mut l = License::blank();
        l.connects.push(Connection::new(Utc::today().naive_utc() + chrono::Duration::days(1)));
        let e = "".to_string();
        let c = Utc::today().naive_utc();
        assert!(l.find(&e, &e, &e,&e).unwrap() > c);
        let user = env::var("USER").unwrap_or("".to_string());
        let h = "localhost".to_string();
        let p = "postgres".to_string();
        let i = "127.0.0.1".to_string();
        test_find_assert(&l, &c, &h, &user);
        test_find_assert(&l, &c, &i, &user);
        test_find_assert(&l, &c, &h, &p);
        test_find_assert(&l, &c, &i, &p);

        let sample_host = "aaaa".to_string();
        let sample_user = "bbbb".to_string();

        l.connects[0].hosts.insert(sample_host.clone());
        l.connects[0].db_names.insert(sample_user.clone());

        test_find_assert(&l, &c, &sample_host, &sample_user);
        test_find_assert(&l, &c, &sample_host, &user);
        test_find_assert(&l, &c, &sample_host, &p);
        test_find_assert(&l, &c, &h, &sample_user);
        test_find_assert(&l, &c, &i, &sample_user);

        l.connects.push(Connection::new(Utc::today().naive_utc() - chrono::Duration::days(10)));
        l.connects[1].hosts.insert(sample_host.clone());
        l.connects[1].db_names.insert(sample_user.clone());

        test_find_assert(&l, &c, &sample_host, &sample_user);
        test_find_assert(&l, &c, &sample_host, &user);
        test_find_assert(&l, &c, &sample_host, &p);
        test_find_assert(&l, &c, &h, &sample_user);
        test_find_assert(&l, &c, &i, &sample_user);

        let c = Utc::today().naive_utc() - chrono::Duration::days(100);
        l.connects.remove(0);

        test_find_assert(&l, &c, &sample_host, &sample_user);
        test_find_assert(&l, &c, &sample_host, &user);
        test_find_assert(&l, &c, &sample_host, &p);
        test_find_assert(&l, &c, &h, &sample_user);
        test_find_assert(&l, &c, &i, &sample_user);
    }

    fn test_find_assert(l: &License, compare_to: &NaiveDate, host: &String, db: &String) {
        let e = "".to_string();
        assert!(l.find(host, &e, &e, &e).unwrap() > *compare_to);
        assert!(l.find(&e, &host, &e, &e).unwrap() > *compare_to);
        assert!(l.find(&e, &e, &db, &e).unwrap() > *compare_to);
        assert!(l.find(&e, &e, &e, &db).unwrap() > *compare_to);

        assert!(l.find(host, &e, &db, &e).unwrap() > *compare_to);
        assert!(l.find(host, &e, &e, &db).unwrap() > *compare_to);
        assert!(l.find(&e, host, &db, &e).unwrap() > *compare_to);
        assert!(l.find(&e, host, &e, &db).unwrap() > *compare_to);
    }

}
