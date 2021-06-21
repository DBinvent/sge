#[allow(unused)]
use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::{env, fs};

use crate::ConfigMap;
use chrono::{DateTime, Utc, NaiveDate, Datelike};
use crypto_hash::Algorithm;
use rand::thread_rng;
use rsa::{BigUint, PaddingScheme, PublicKey, PublicKeyParts, RSAPrivateKey, RSAPublicKey};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use slog::Logger;
use std::collections::HashMap;
use yaml_rust::{Yaml, YamlLoader};
use std::cmp::max;
use std::time::{UNIX_EPOCH, SystemTime};

static PKS: [&'static str; 10] = [
include_str!("keys/corp0.pub"),
include_str!("keys/corp1.pub"),
include_str!("keys/corp2.pub"),
include_str!("keys/corp3.pub"),
include_str!("keys/corp4.pub"),
include_str!("keys/corp5.pub"),
include_str!("keys/corp6.pub"),
include_str!("keys/corp7.pub"),
include_str!("keys/corp8.pub"),
include_str!("keys/corp9.pub"),
];

const INV_LL: &str =
    "Please check https://dbinvent.com/rdbm-license.html See './rdbm help license' for more details.";

#[allow(dead_code)]
const API_HOST: &str = "https://dbinvent.com";
// const ABOUT_EXPIRE: u8 = 17; // days
const EVALUATION_LICENSE: &str = "evaluation.license";
const LICENSE_FILE: &str = "rdbm.license";

/// on license action
pub fn license(cfg: &ConfigMap, log: &Logger, test: Option<&dyn Fn(&ApiRequest) -> String>) -> Result<(), String> {
    let email = cfg.get("email");
    let dry_run = cfg.get_bool("dry_run", false);
    let license_file = match cfg.get_opt(LICENSE_FILE) {
        None => EVALUATION_LICENSE.to_string(),
        Some(f) => if Path::new(f.as_str()).is_file() { f } else {
            EVALUATION_LICENSE.to_string()
        }
    };

    let mut subsr: BTreeSet<String> = BTreeSet::new();
    cfg.get("subscribe").split(",").for_each(|s| {
        subsr.insert(s.into());
    });
    let db_host = cfg.get_opt("db_host").unwrap_or("".into());
    let logdb_host = cfg.get_opt("logdb_host").unwrap_or("".into());
    let db_name = cfg.get_opt("db_name").unwrap_or("".into());
    let logdb_name = cfg.get_opt("logdb_name").unwrap_or("".into());

    let do_subscribe = subsr.contains("*")
        || (db_host.len() > 0 && subsr.contains(&db_host))
        || (logdb_host.len() > 0 && subsr.contains(&logdb_host))
        || (db_name.len() > 0 && subsr.contains(&db_name))
        || (logdb_name.len() > 0 && subsr.contains(&logdb_name));
    let conn_set = db_host.len() + logdb_host.len() + db_name.len() + logdb_name.len() > 0;
    let eval_set = license_file.as_str() == EVALUATION_LICENSE;

    if Path::new(license_file.as_str()).is_file() {
        match License::load(license_file.as_str()) {
            Ok(l) => match l.verify() {
                Ok(_) => {
                    info!( log, "License [{}] issued to: {} {}{}", license_file, l.issued_to, l.email);

                    if l.find(&db_host, &logdb_host, &db_name, &logdb_name).is_some()
                        || (!conn_set && eval_set) {
                        if dry_run || eval_set {
                            info!(log, "Good to use for your connection(s)! Thank you! ");
                            Ok(())
                        } else {
                            debug!(log, "Trying to pull license to update ...");
                            ApiRequest::Pull(l).call(&license_file, dry_run, log, test)
                        }
                    } else if email.len() > 0 && do_subscribe {
                        let lc = CreateUpdateRequest::new(&email, &db_host, &logdb_host, &db_name, &logdb_name)?;
                        ApiRequest::Update(l, lc).call(&license_file, dry_run, log, test)
                    } else if conn_set {
                        Err(format!("The license loaded from file [{}] is valid, but not for configuration set, check your configuration and license file. Also,{}", license_file, INV_LL))
                    } else {
                        Err(format!("The license loaded from file [{}] is valid, but no configuration set to check! {}", license_file, INV_LL))
                    }
                }
                Err(e) => Err(format!(
                    "The license loaded from file [{}] is not valid: {}. {}",
                    license_file, e, INV_LL
                )),
            },
            Err(e) => Err(format!(
                "The license not loaded from file [{}]: {}. {}",
                license_file, e, INV_LL
            )),
        }
    } else if email.len() > 0 && do_subscribe {
        let lc = CreateUpdateRequest::new(&email, &db_host, &logdb_host, &db_name, &logdb_name)?;
        ApiRequest::Create(lc).call(&license_file, dry_run, log, test)
    } else {
        Err(format!("No '--email' nor '--license_file' set, also might be helpful: --subscribe=* and db connections to set. Please check './rdbm help license' for more details."))
    }
}

pub enum ApiRequest {
    Create(CreateUpdateRequest),
    Update(License, CreateUpdateRequest),
    Pull(License),
}

impl ApiRequest {
    #[inline]
    pub fn method(&self) -> &str {
        match &self {
                ApiRequest::Create(_) => "creating",
                ApiRequest::Pull(_) => "checking",
                ApiRequest::Update(_,_) => "updating",
            }
    }

    #[inline]
    fn call(&self, file: &String, dry_run: bool, log: &Logger, test: Option<&dyn Fn(&ApiRequest) -> String>
    ) -> Result<(), String> {
        let url = format!("{}/api/license", api_host());
        let method = &self.method();
        if dry_run {
            info!(log, "About to call {} to {} license, but --dry_run set.", url, method);
            return Ok(());
        }
        let auth_header = "Authentication";
        let mut encm = match &self {
            ApiRequest::Update(l, _lu) => l.encrypt_all()?,
            ApiRequest::Pull(l) => l.encrypt_all()?,
            ApiRequest::Create(_) => HashMap::new()
        };
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
                        let request = serde_json::to_string(&lu.clean(&mut encm, &l)?)
                            .map_err(|e| format!("creating payload of a license create request: {}", e))?;
                        ureq::put(url.as_str())
                            .set(auth_header, l.header()?.as_str())
                            .send_string(request.as_str())
                            .map_err(|e| format!("{} license: {}", method, e))?
                    }
                    ApiRequest::Pull(l) => {
                        ureq::get(format!("{}/{}", url, l.api_key).as_str())
                            .set(auth_header, l.header()?.as_str())
                            .call()
                            .map_err(|e| format!("{} license: {}", method, e))?
                    }
                };
                let st = response.status();
                let r = response.into_string().map_err(|e| format!("getting response: {}", e))?;
                trace!(log, "response [{}] from {} at {}\n{}", st, method, url, r);
                r
            }
            Some(callback) => {
                callback(&self)
            },
        };

        match self {
            ApiRequest::Create(_l) => {
                let response: ApiResponse = serde_json::from_str(response.as_str())
                    .map_err(|e| format!("converting response on create: {} from {}", e, response))?;
                if response.success {
                    // println!("{}", response.result);
                    Ok(())
                } else { Err(as_err(&response.errors, &response.messages)) }
            }
            ApiRequest::Update(l, _) => LicenseResponse::save(file, response, l, encm),
            ApiRequest::Pull(l) => LicenseResponse::save(file, response, l, encm),
        }
    }
}

impl LicenseResponse {
    #[inline]
    fn save(file: &String, response: String, license: &License,
        encoded_map: HashMap<String, String>
        ) -> Result<(), String> {
        let lr: LicenseResponse = serde_json::from_str(response.as_str())
            .map_err(|e| format!("converting response on save from api response: {}, with error: {}", response, e))?;
        match lr.result {
            None => Err(as_err(&lr.errors, &lr.messages)),
            Some(lr) => {
                let mut lr = lr.convert();
                lr.patch(license, &encoded_map);
                lr.verify()?;
                fs::write(file.clone(), serde_yaml::to_string(&lr)
                    .map_err(|e| format!("creating license: {}", e))?)
                    .map_err(|e| format!("writing to [{}] {}", file, e))?;
                Ok(())
            }
        }

    }

}

#[cfg(debug_assertions)]
fn api_host() -> String {
    "http://localhost:8000".to_string()
}

#[cfg(not(debug_assertions))]
#[inline]
fn api_host() -> String {
    API_HOST.to_string()
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

    #[inline]
    fn clean(&self, encm: &mut HashMap<String, String>, lic: &License) -> Result<Self, String> {
        let public_key = lic.pubk()?;
        Ok(CreateUpdateRequest {
            personal: self.personal,
            email: self.email.clone(),
            hosts: clean_list(&self.hosts, encm, &public_key)?,
            db_names: clean_list(&self.db_names, encm, &public_key)?,
        })
    }

}

#[inline]
fn clean_list(list: &Vec<String>, encm: &mut HashMap<String, String>, public_key: &RSAPublicKey) -> Result<Vec<String>, String> {
    let mut res = Vec::new();
    for item in list {
        let encoded = Connection::encode(item, &public_key)?;
        encm.insert(encoded.clone(), item.clone());
        res.push(encoded);
    }
    Ok(res)
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
    result: Option<JopLicense>,
    success: bool,
    errors: Vec<ApiMessage>,
    messages: Vec<ApiMessage>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct ApiResponse {
    // result: String,
    success: bool,
    errors: Vec<ApiMessage>,
    messages: Vec<ApiMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiMessage {
    pub code: i32,
    pub message: String
}

fn as_err(errors: &Vec<ApiMessage>, messages: &Vec<ApiMessage>) -> String {
    let mut res = String::new();
    for e in errors {
        if res.len() > 0 {res.push_str(", ");}
        res.push_str(e.message.as_str());
    }
    for e in messages {
        if res.len() > 0 {res.push_str(", ");}
        res.push_str(e.message.as_str());
    }
    res
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
    pub issued_to: String, //"Evaluate by"
    #[serde(skip_serializing_if = "String::is_empty")]
    pub email: String,     //"@gmail.com"
    pub notice: String,    //"это поле свободно редактируется"
    pub connects: Vec<Connection>,
    #[serde(skip)]
    pub is_conn_encrypt: bool, // connections содержит хеши имен (len>20 & starts ~)
    #[serde(with = "rfc3339")]
    pub api_key_valid_until: LicenseTime, //"2021-12-12"
    #[serde(skip_serializing_if = "String::is_empty")]
    pub api_key: String, // "<jwt token 300-500char>"
    /// package name TODO add license key version suffix
    pub license_type: String,
    #[serde(skip)]
    pub version: u8,
    pub license: String, //"<signature_see_bellow -- это будет строчка 300-500 символов>"
    /// will be generated by openssl if connections are not encrypted AND 'cpk' not set
    #[serde(skip_serializing_if = "String::is_empty")]
    pub key: String, //"<this client  RSA private key 40 строк х64char>"
    #[serde(skip)]
    /// clients RSA Pub key: use to encode connections, if they are not
    pub cpk: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    #[serde(with = "local_date_format")]
    pub valid_until: LicenseDate,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub hosts: BTreeSet<String>,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
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
        if is_encrypted(input) {
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
        if is_encrypted(input) || (input.len() == 1 && input.as_bytes()[0] == b'*') {
            Ok(input.clone())
        } else {
            let mut res = base64::encode(Connection::encrypt(input, &public_key)?);
            res.insert(0, '~');
            Ok(res)
        }
    }

}

#[inline]
fn is_encrypted(input: &String) -> bool {
    input.len() > 24 && input.as_bytes()[0] == b'~'
}

#[inline]
fn is_encrypted_set(input: &BTreeSet<String>) -> bool {
    for s in input {
        if is_encrypted(s) {
            return true;
        }
    }
    false
}

impl License {
    #[allow(dead_code)]
    pub fn blank() -> Self {
        License {
            issued_to: "".to_string(),
            email: "".to_string(),
            notice: "".to_string(),
            connects: vec![],
            is_conn_encrypt: false,
            api_key_valid_until: Utc::now().naive_utc(),
            api_key: "".to_string(),
            license_type: "".to_string(),
            version: 0,
            license: "".to_string(),
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
    pub fn digest(&self) -> Result<Vec<u8>, String> {
        let public_key = self.pubk();
        let mut hasher = crypto_hash::Hasher::new(Algorithm::SHA512);
        let _ = hasher.write(self.issued_to.as_bytes());
        let _ = hasher.write(self.email.as_bytes());
        let _ = hasher.write(self.license_type.as_bytes());
        if self.license_type.rfind("_v").is_none() {
            let lv = format!("_v{}", self.version);
            let _ = hasher.write(lv.as_bytes());
        }
        for c in &self.connects {
            let _ = hasher.write(&c.valid_until.year().to_be_bytes());
            let _ = hasher.write(&c.valid_until.num_days_from_ce().to_be_bytes());
            if c.hosts.len() > 0 || c.db_names.len() > 0 {
                // evaluation license does not have a key
                match &public_key {
                    Ok(p) => {
                        for e in &c.encrypt_all(&p)? {
                            let _ = hasher.write(e.as_ref());
                        }
                    }
                    Err(e) => { return Err(e.to_owned()); }
                }
            }
        }
        let d = hasher.finish();
        let d = Sha1::digest(&d).to_vec();
        Ok(d)
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
                    let mut is_conn_encrypt = false;
                    let mut connects = Vec::new();
                    if let Some(cv) = y["connects"].as_vec() {
                        for c in cv {
                            let (hosts, encr) = to_list(c, "hosts");
                            is_conn_encrypt = is_conn_encrypt || encr;
                            let (db_names, encr) =  to_list(c, "db_names");
                            is_conn_encrypt = is_conn_encrypt || encr;
                            connects.push(Connection {
                                valid_until: to_date(c, "valid_until")?,
                                hosts,
                                db_names,
                            });
                        }
                    }
                    let (license_type, version) =  License::parse_version(yaml_as_str(y, "license_type"));
                    Ok(License {
                        license_type,
                        issued_to: yaml_as_str(y, "issued_to"),
                        email: yaml_as_str(y, "email"),
                        notice: yaml_as_str(y, "notice"),
                        connects,
                        is_conn_encrypt,
                        api_key_valid_until: to_time(y, "api_key_valid_until")?,
                        api_key: yaml_as_str(y, "api_key"),
                        license: yaml_as_str(y, "license"),
                        key: yaml_as_str(y, "key"),
                        cpk: None,
                        version
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
    pub fn parse_version(input: String) -> (String, u8) {
        match input.rfind("_v") {
            None => (input, 0),
            Some(i) => ( input[..i].to_owned(),

            if i + 1 > input.len() { 0 } else {
                input[i + 2..].parse::<u8>().unwrap_or(0)
            })
        }
    }


    #[allow(unused)]
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
                let pubk = self.corp_pub_key(self.version as usize)?;
                let d= self.digest()?;
                #[cfg(debug_assertions)]
                crc::debug("verify_signd", &d, &sign, &pubk);
                match pubk.verify(
                    PaddingScheme::new_pss::<Sha1, _>(thread_rng()),
                    &d,
                    &sign,
                ) {
                    Ok(_) => Ok(()),
                    Err(e) => Err(format!("license: {}", e)),
                }
            }
            Err(e) => Err(format!("reading license: {}", e)),
        }
    }
    // License

    #[inline]
    fn as_ver(pubk: &RSAPublicKey) -> u8 {
        License::as_vers(base64::encode(pubk.n().to_bytes_be()).as_str() ) + 1
    }

    #[inline]
    fn as_vers(pubk: &str) -> u8 {
       let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().subsec_nanos().to_string();
       pubk[0..1].to_string().parse::<u8>().unwrap_or(n[n.len()-2..].parse::<u8>().unwrap_or(0))
    }

    /// prepare digest calculation
    #[inline]
    pub fn pre_digest(&mut self, pubk: &RSAPublicKey) -> &Self {
        self.version = License::as_ver(&pubk);
        let (lt, _) = License::parse_version(self.license_type.clone());
        self.license_type = format!("{}_v{}", lt, self.version);
        self
    }

    #[inline]
    fn corp_pub_key(&self, version: usize) -> Result<RSAPublicKey, String> {
        if version > PKS.len() {
            Err(format!("Please upgrade. The license version is not supported yet: {}", version))
        } else {
            // небольшая проверка по коду версии :)
            let maxv = PKS.len();
            let version = max(1, version);
            let p = License::as_vers(&PKS[version-1 % maxv][..1]) as usize + 1;
            let version = version - 1 + (p % version);
            str2pub(&PKS[version % maxv].into())
        }
    }

    #[inline]
    #[allow(dead_code)]
    pub(crate) fn rewrite(&self, input: String, file: &str) -> Result<(), String> {
        let mut l = License::new(input, "https://dbinvent.com/")?;
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
    #[allow(dead_code)]
    pub fn is_evaluate(&self) -> bool {
        for c in &self.connects {
            if c.hosts.len() == 0 && c.db_names.len() == 0 {
                return true;
            }
        }
        false
    }

    #[inline]
    #[allow(dead_code)]
    pub fn is_appendable(&self, valid: &LicenseDate) -> bool {
        for c in &self.connects {
            if &c.valid_until == valid {
                return true;
            }
        }
        false
    }
    // License

    /// opposite to clean
    #[inline]
    fn patch(&mut self, original: &License, enc: &HashMap<String, String>) {
        self.notice = original.notice.clone();
        self.key = original.key.clone();
        for c in &mut self.connects {
            License::patch_set(&mut c.hosts, enc);
            License::patch_set(&mut c.db_names, enc);
        }
    }
    // License

    fn patch_set(dest: &mut BTreeSet<String>, enc: &HashMap<String, String>) {
        let original = dest.clone();
        dest.clear();
        let ol = original.len();
        for name in original {
            let decoded = match enc.get(&name) {
                None => &name,
                Some(d) => d,
            }.to_owned();
            println!("patching[{}/{}:{}] {}->{}", dest.len()+1, ol, enc.len(), name, decoded);
            dest.insert(decoded);
        }
    }

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
        let pubk = match self.pubk() {
            Ok(p) => Some(p),
            Err(_e) => if self.is_conn_encrypt {
                return None;
            } else {
                None
            }
        };
        let e_host_a = self.encrypt_name(host_a, &pubk);
        let e_host_b = self.encrypt_name(host_b, &pubk);
        let e_db_a = self.encrypt_name(db_a, &pubk);
        let e_db_b = self.encrypt_name(db_b, &pubk);

        for c in &self.connects {
            if (c.hosts.contains("*")
                || ((is_ip_a || c.hosts.contains(host_a) || c.hosts.contains(&e_host_a))
                    && (is_ip_b || c.hosts.contains(host_b)) || c.hosts.contains(&e_host_b)))
                && (((db_a == "" || db_a == user || db_a == &pg)
                     && (db_b == "" || db_b == user || db_b == &pg))
                    || c.db_names.contains("*")
                    || ((c.db_names.contains(db_a) || c.db_names.contains(&e_db_a))
                        && (db_b.len() == 0 || c.db_names.contains(db_b) || c.db_names.contains(&e_db_b))))
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

    #[inline]
    fn encrypt_all(&self) -> Result<HashMap<String, String>, String> {
        let mut encm = HashMap::new();
        let public_key = self.pubk()?;
        for c in &self.connects {
            for i in &c.hosts { encm.insert(Connection::encode(i, &public_key)?, i.clone()); }
            for i in &c.db_names { encm.insert(Connection::encode(i, &public_key)?, i.clone()); };
        }
        Ok(encm)
    }

    #[inline]
    fn encrypt_name(&self, input: &String, pubk: &Option<RSAPublicKey>) -> String {
        if self.is_conn_encrypt {
            match pubk {
                None => "*".into(),
                Some(pubk) => {
                    match Connection::encode(input, pubk) {
                        Ok(e) => e,
                        Err(e) => {
                            eprintln!("Encoding {}: {}", input, e);
                            e.to_string()
                        }
                    }
                }
            }
        } else {
            "*".into()
        }
    }

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
                Ok(s) => {
                    // crc::debug(" _ header", &d, &s,  &rsa.to_public_key());
                    Ok(base64::encode(s))
                },
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
                let pubk = self.pubk()?;
                // crc::debug("is_header", &d, &sign, &pubk);
                pubk.verify(PaddingScheme::new_pss::<Sha1, _>(thread_rng()), &d, &sign)
                    .map_err(|e| format!("invalid auth header: {}", e))
            }
            Err(e) => Err(format!("reading header [{}]: {}", auth.len(), e)),
        }
    }
    // License
/*
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
*/
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

#[allow(dead_code)]
pub mod crc {
    use std::fmt::Write;
    use rsa::{RSAPublicKey, PublicKeyParts};

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

    #[cfg(not(debug_assertions))]
    #[inline]
    pub fn debug(method: &str, d: &[u8],  ds: &[u8], pubk: &RSAPublicKey) { }

    #[cfg(debug_assertions)]
    #[inline]
    pub fn debug(method: &str, digest: &[u8], signature: &[u8], pubk: &RSAPublicKey) {
        let mut s = String::new();
        a(&mut s, digest);
        s.push_str(".pub");
        a(&mut s, &pubk.n().to_bytes_be());
        s.push_str(".sig[");
        s.push_str(signature.len().to_string().as_str());
        s.push_str("]");
        a(&mut s, signature);
        println!("{}.crc:digest[{}]{}", method, digest.len(), s);
    }

}

/// Json Option Response mapping
#[derive(Debug, Deserialize)]
pub struct JopLicense {
    pub issued_to: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub email: Option<String>,
    pub connects: Vec<JopConnection>,
    #[serde(with = "rfc3339")]
    pub api_key_valid_until: LicenseTime, //"2021-12-12"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    pub license_type: String,
    pub license: String,
    /// will be generated by openssl if connections are not encrypted AND 'cpk' not set
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>, //"<this client  RSA private key 40 строк х64char>"
}

#[derive(Debug,Deserialize)]
pub struct JopConnection {
    #[serde(with = "local_date_format")]
    pub valid_until: LicenseDate,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hosts: Option<BTreeSet<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db_names: Option<BTreeSet<String>>,
}

impl JopConnection {
    fn convert(self, avoid_encr_check: bool) -> (Connection, bool) {
        let mut is_conn_encrypt = avoid_encr_check;
        let mut hosts = BTreeSet::new();
        if let Some(mut set) = self.hosts {
            is_conn_encrypt = is_conn_encrypt || is_encrypted_set(&set);
            hosts.append(&mut set);
        }
        let mut db_names = BTreeSet::new();
        if let Some(mut set) = self.db_names {
            is_conn_encrypt = is_conn_encrypt || is_encrypted_set(&set);
            db_names.append(&mut set);
        }
        (Connection {
            valid_until: self.valid_until,
            hosts,
            db_names
        }, is_conn_encrypt)
    }
}

impl JopLicense {
    fn convert(self) -> License {
        let mut is_conn_encrypt = false;
        let mut connects = Vec::new();
        for c in self.connects {
            let (con, encr) = c.convert(is_conn_encrypt);
            is_conn_encrypt = is_conn_encrypt || encr;
            connects.push(con);
        }
        License {
            issued_to: self.issued_to,
            email: self.email.unwrap_or("".to_string()),
            notice: "".to_string(),
            connects,
            is_conn_encrypt,
            api_key_valid_until: self.api_key_valid_until,
            api_key: self.api_key.unwrap_or("".to_string()),
            license_type: self.license_type,
            version: 0,
            license: self.license,
            key: self.key.unwrap_or("".to_string()),
            cpk: None
        }
    }
}

#[inline]
fn to_list(yaml: &Yaml, input: &str) -> (BTreeSet<String>, bool) {
    let mut encrypted = false;
    let mut r = BTreeSet::new();
    if let Some(list) = yaml[input].as_vec() {
        for l in list {
            if let Some(c) = l.as_str() {
                let c = c.to_string();
                encrypted = encrypted || is_encrypted(&c);
                r.insert(c);
            }
        }
    }
    (r, encrypted)
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
            Ok(be) => {
                RSAPublicKey::new(
                    BigUint::from_bytes_be(bn.as_ref()),
                    BigUint::from_bytes_be(be.as_ref()),
                )
            },
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
                Err(format!("input is too small for PKCS1: {}", der_bytes.len()))
            }
        }
        Err(e) => Err(e.to_string()),
    }
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
    fn init_elic() -> Result<(), String> {
        if Path::new(EVALUATION_LICENSE).is_file() {
            License::load(EVALUATION_LICENSE)?.verify();
        }
        Ok(())
    }

    #[test]
    fn init_lic() -> Result<(), String> {
        let lf = "tests/signed_test_rdbm.license";
        if Path::new(lf).is_file() {
            let l = License::load(lf)?;
            l.verify()?;
            let h = l.header()?;
            assert!(h.len() > 0);
            l.is_header(&h)?;
        }
        Ok(())
    }

    #[test]
    fn clean_lr() -> Result<(), String> {
        let mut l = License::blank();
        let now: LicenseDate = Utc::today().naive_utc();
        l.cpk = Some(PKS[0].into());
        let cr = CreateUpdateRequest {
            personal: false,
            email: "em".to_string(),
            hosts: vec!["host".into()],
            db_names: vec!["db".into()]
        };
        let mut encm = HashMap::new();
        let cc = cr.clean(&mut encm, &l)?;
        assert_eq!(encm.len(), 2);
        assert_ne!(cc.db_names[0], cr.db_names[0]);
        assert_eq!(encm.get(&cc.db_names[0]).unwrap_or(&"".to_string()), &cr.db_names[0]);
        assert_ne!(cc.hosts[0], cr.hosts[0]);
        assert_eq!(encm.get(&cc.hosts[0]).unwrap_or(&"".to_string()), &cr.hosts[0]);
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
        test_find_assert_gt(&l, &c, &h, &user);
        test_find_assert_gt(&l, &c, &i, &user);
        test_find_assert_gt(&l, &c, &h, &p);
        test_find_assert_gt(&l, &c, &i, &p);

        let sample_host = "aaaa".to_string();
        let sample_user = "bbbb".to_string();

        l.connects[0].hosts.insert(sample_host.clone());
        l.connects[0].db_names.insert(sample_user.clone());

        test_find_assert_gt(&l, &c, &sample_host, &sample_user);
        test_find_assert_gt(&l, &c, &sample_host, &user);
        test_find_assert_gt(&l, &c, &sample_host, &p);
        test_find_assert_gt(&l, &c, &h, &sample_user);
        test_find_assert_gt(&l, &c, &i, &sample_user);

        l.connects.push(Connection::new(Utc::today().naive_utc() - chrono::Duration::days(10)));
        l.connects[1].hosts.insert(sample_host.clone());
        l.connects[1].db_names.insert(sample_user.clone());

        test_find_assert_gt(&l, &c, &sample_host, &sample_user);
        test_find_assert_gt(&l, &c, &sample_host, &user);
        test_find_assert_gt(&l, &c, &sample_host, &p);
        test_find_assert_gt(&l, &c, &h, &sample_user);
        test_find_assert_gt(&l, &c, &i, &sample_user);

        let c = Utc::today().naive_utc() - chrono::Duration::days(100);
        l.connects.remove(0);

        test_find_assert_gt(&l, &c, &sample_host, &sample_user);
        test_find_assert_gt(&l, &c, &sample_host, &user);
        test_find_assert_gt(&l, &c, &sample_host, &p);
        test_find_assert_gt(&l, &c, &h, &sample_user);
        test_find_assert_gt(&l, &c, &i, &sample_user);
    }

    fn test_find_assert_gt(l: &License, compare_to: &NaiveDate, host: &String, db: &String) {
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

    fn load(rsaf: &String) -> Result<RSAPrivateKey, String> {
        let input = fs::read_to_string(rsaf.clone())
            .map_err(|e| format!("read file {}: {}", rsaf, e))?
            .lines()
            .filter(|line| !line.starts_with("-"))
            .fold(String::new(), |mut data, line| {
                data.push_str(&line);
                data
            });
        let encoded = base64::decode(&input)
            .map_err(|e| format!("key decode error [{}] {} ", rsaf, e))?;
        RSAPrivateKey::from_pkcs8(&encoded)
                .map_err(|e| format!("key create error [{}] {:?}", rsaf, e))
    }

    #[test]
    fn test_vers() -> Result<(), String>  {
        assert!(License::as_vers("21") < 10);
        let l = License::blank();
        for i in 0..PKS.len()-1 {
            let p = PKS[i];
            assert_eq!(p.len(), 516);
            assert_eq!(p[512..], "AQAB".to_string());
            assert_eq!(i, License::as_vers(PKS[i]) as usize);
            assert_eq!(i + 1, License::as_ver(&l.corp_pub_key(i+1)?) as usize, "version#{}", i);
            let rsaf = format!("keys/corp{}.rsa", i);
            if Path::new(&rsaf).is_file() {
                let pf = load(&rsaf)?;
                assert_eq!(pub2str(&pf).as_str(), p);
            }
        }
        Ok(())
    }

    #[test]
    fn test_find_dbn_1() {
        let user = env::var("USER").unwrap_or("".to_string());
        let e = "".to_string();
        let h = "localhost".to_string();
        let p = "postgres".to_string();
        let i = "127.0.0.1".to_string();
        let zzz = "zzz".to_string();

        let mut l = License::blank();
        l.connects.push(Connection::new(Utc::today().naive_utc() + chrono::Duration::days(1)));
        l.connects[0].db_names.insert("*".into());
        //only db name
        assert!(l.find(&i, &h, &zzz, &p).is_some());
        assert!(l.find(&i, &h, &user, &zzz).is_some());
        assert!(l.find(&zzz, &h, &zzz, &p).is_none());
        assert!(l.find(&i, &zzz, &p, &zzz).is_none());

        // expiring license
        l.connects[0].valid_until = Utc::today().naive_utc() - chrono::Duration::days(1);
        l.connects.push(Connection::new(Utc::today().naive_utc() + chrono::Duration::days(1)));
        // expired license
        assert!(l.find(&i, &h, &zzz, &p).unwrap() < Utc::today().naive_utc());
        assert!(l.find(&i, &h, &user, &zzz).unwrap() < Utc::today().naive_utc());
        assert!(l.find(&zzz, &h, &zzz, &p).is_none());
        assert!(l.find(&i, &zzz, &p, &zzz).is_none());
        // eval license
        assert!(l.find(&i, &h, &user, &e).unwrap() > Utc::today().naive_utc());
        l.connects[1].hosts.insert("*".into());
        // new license
        assert!(l.find(&i, &h, &zzz, &p).unwrap() < Utc::today().naive_utc());
        assert!(l.find(&i, &h, &user, &zzz).unwrap() < Utc::today().naive_utc());
        assert_eq!((l.find(&zzz, &h, &user, &p).unwrap() - Utc::today().naive_utc()).num_days(), 1);
        assert_eq!((l.find(&i, &zzz, &p, &e).unwrap() - Utc::today().naive_utc()).num_days(), 1);

    }

    #[test]
    fn test_pvers() {
        let (x, y) = License::parse_version("x_v21".to_string());
        assert_eq!(x, x);
        assert_eq!(y, 21);
    }


    #[test]
    fn pubk() -> Result<(), String> {
		let l = Path::new("keys");
		if l.is_dir() {
			for f in l.read_dir().map_err(|e| format!("{}", e))? {
				let fi = f.unwrap().path().to_string_lossy().to_string();
				if fi.ends_with(".rsa") {
                    let rsa = load(&fi)?;
                    let id = (License::as_ver(&rsa.to_public_key()) as usize) - 1;
					println!("{} PUB:[{}:{}...]", fi, id, pub2str(&rsa.to_public_key())[..16].to_string());
                    assert_eq!(PKS[id], pub2str(&rsa.to_public_key()).as_str());
                    assert_eq!(id, fi[fi.len()-5..fi.len()-4].to_string().parse::<u8>().unwrap() as usize);
				}
			}
		}
        Ok(())
    }

    #[test]
    fn test_json() {
        for response in &SAMPLES {
            let lr: ApiResponse = serde_json::from_str(response).unwrap();
            let lr: LicenseResponse = serde_json::from_str(response).unwrap();
        }
    }

const SAMPLES: [&'static str; 2] = [r###"
{
  "result": {
    "api_key_valid_until": "2022-07-19T04:03:48.007857+00:00",
    "connects": [{
        "valid_until": "2022-07-22"
      }
    ],
    "issued_to": "Evaluate by 925com",
    "license": "RpCJ",
    "license_type": "Standard_v1",
    "notice": ""
  },
  "success": true,
  "errors": [],
  "messages": []
}
"###,
r###"
{
  "result": {
    "api_key": "9e04a183-acff-489d-8177-fb13afeade48",
    "api_key_valid_until": "2022-07-19T04:03:48.007857+00:00",
    "connects": [
      {
        "db_names": [
          "~OSKI1KjFT4LZKgmQjIFfiq8Ywsk="
        ],
        "valid_until": "2022-07-22"
      },
      {
        "hosts": [
          "~x3nzzDJtyr7C3zP18njCu0Njbww="
        ],
        "valid_until": "2022-07-22"
      }
    ],
    "email": "92596725@dbinvent.com",
    "issued_to": "Evaluate by 925com",
    "license": "RpCJ",
    "license_type": "Standard_v1",
    "notice": ""
  },
  "success": true,
  "errors": [],
  "messages": []
}
"###,
];

}

