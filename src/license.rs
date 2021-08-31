#[allow(unused)]
use std::collections::BTreeSet;
use std::path::Path;
use std::{env, fs};

use crate::{TR, ConfigMap};
use chrono::{DateTime, Utc, NaiveDate, Datelike};
use rand::thread_rng;
use rsa::{BigUint, PublicKey, PublicKeyParts, RSAPrivateKey, RSAPublicKey};
use serde::{Deserialize, Serialize};
use slog::Logger;
use yaml_rust::{Yaml, YamlLoader};
use std::cmp::max;
use std::time::{UNIX_EPOCH, SystemTime};
use std::mem::swap;
use crypto::digest::Digest;
use rsa::padding::PaddingScheme::{PKCS1v15Sign, PKCS1v15Encrypt};
use rsa::hash::Hash::{SHA2_512, SHA2_256};

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

#[allow(dead_code)]
const API_HOST: &str = "https://dash.dbinvent.com";
const EVALUATION_LICENSE: &str = "evaluation.license";
const LICENSE_FILE: &str = "license_file";
// minimal allowed version (key compromised)
const MIN_VER: usize = 1usize;
const PG: &str = "postgres";
const LH: &str = "localhost";
const STAR: &str = "*";

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

    let do_subscribe = subsr.contains(&"*".to_string())
        || (db_host.len() > 0 && subsr.contains(&db_host))
        || (logdb_host.len() > 0 && subsr.contains(&logdb_host))
        || (db_name.len() > 0 && subsr.contains(&db_name))
        || (logdb_name.len() > 0 && subsr.contains(&logdb_name));
    let conn_set = db_host.len() + logdb_host.len() + db_name.len() + logdb_name.len() > 0;
    let eval_file = license_file.as_str() == EVALUATION_LICENSE;

    if Path::new(license_file.as_str()).is_file() {
        match License::load(license_file.as_str()) {
            Ok(l) => match l.verify() {
                Ok(_) => {
                    info!(log, "{}", TR.t7ea("license", "info", &["{file}", "{to}", "{email}"], &[&license_file, &l.issued_to, &l.email]));
                    let found = l.find(&db_host, &logdb_host, &db_name, &logdb_name);
                    if found.is_some() {
                        if dry_run || eval_file {
                            info!(log, "{}", TR.t7ea("license", "good", &[], &[]));
                            Ok(())
                        } else {
                            info!(log, "{}", TR.t7ea("license", "pulling", &["{license}"], &[&l.api_key]));
                            ApiRequest::Pull(l).call(&license_file, dry_run, log, test)
                        }
                    } else if found.is_none() && do_subscribe {
                        let lc = CreateUpdateRequest::new(
                            if l.email.len() == 0 {
                                email.clone()
                            } else {
                                l.email.clone()
                            }, &cfg, &db_host, &logdb_host, &db_name, &logdb_name)?;
                        if eval_file && email.len() > 0   {
                            info!(log, "{}", TR.t7ea("license", "creating", &[], &[]));
                            ApiRequest::Create(lc).call(&license_file, dry_run, log, test)
                        } else {
                            info!(log, "{}", TR.t7ea("license", "updating", &["{license}"], &[&l.api_key]));
                            ApiRequest::Update(l, lc).call(&license_file, dry_run, log, test)
                        }
                    } else if conn_set {
                        let code = if l.connections.len() > 0 && l.connections[l.connections.len()-1].state_code() == Connection::UNPAID {
                            "err0"
                        } else {
                            "err1"
                        };
                        Err(TR.t7ea("license", code, &["{file}", "{$invalid}"], &[&license_file]))
                    } else {
                        Err(TR.t7ea("license", "err2", &["{file}", "{$invalid}"], &[&license_file]))
                    }
                }
                Err(e) => Err(TR.t7ea("license", "err3", &["{file}", "{err}", "{$invalid}"], &[&license_file, &e]))
            },
            Err(e) => Err(TR.t7ea("license", "err4", &["{file}", "{err}", "{$invalid}"], &[&license_file, &e]))
        }
    } else {
        // файла нет, но есть: емаил, подписка, коннекты
        if do_subscribe && email.len() > 0 && !dry_run && conn_set {
            info!(log, "{}", TR.t7ea("license", "creating", &[], &[]));
            ApiRequest::Create( CreateUpdateRequest::new(email, &cfg, &db_host, &logdb_host, &db_name, &logdb_name)?)
                .call(&license_file, dry_run, log, test)
        } else {
            Err(TR.t7ea("license", "err5", &[], &[]))
        }
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
    pub fn m_tr(&self) -> String {
        match &self {
                ApiRequest::Create(_) => TR.t7ea("license", "call_create", &[], &[]),
                ApiRequest::Pull(_) => TR.t7ea("license", "call_check", &[], &[]),
                ApiRequest::Update(_,_) => TR.t7ea("license", "call_update", &[], &[]),
            }
    }

    #[inline]
    fn call(&self, file: &String, dry_run: bool, log: &Logger, test: Option<&dyn Fn(&ApiRequest) -> String>
    ) -> Result<(), String> {
        let url = format!("{}/api/license/", api_host());
        let method = self.method();
        if dry_run {
            info!(log, "{}", TR.t7ea("license", "call_dry", &["{url}", "{call}"], &[&url, &method.to_string()]));
            return Ok(());
        }
        let auth_header = "Authentication";

        let mn = self.m_tr(); // method name
        let response = match test {
            None => {
                let response = match &self {
                    ApiRequest::Create(lc) => {
                        let request = serde_json::to_string(lc)
                            .map_err(|e| TR.t7ea("license", "err6", &["{call}", "{err}"], &[&mn, &e.to_string()]))?;
                        ureq::post(url.as_str())
                            .send_string(request.as_str())
                            .map_err(|e| TR.t7ea("license", "err7", &["{call}", "{err}"], &[&mn, &e.to_string()]))?
                    }
                    ApiRequest::Update(l, lu) => {
                        let request = serde_json::to_string(&lu) //.clean(&l)?)
                            .map_err(|e| TR.t7ea("license", "err6", &["{call}", "{err}"], &[&mn, &e.to_string()]))?;
                        ureq::put(format!("{}{}", url, l.api_key).as_str())
                            .set(auth_header, l.header()?.as_str())
                            .send_string(request.as_str())
                            .map_err(|e| TR.t7ea("license", "err7", &["{call}", "{err}"], &[&mn, &e.to_string()]))?
                    }
                    ApiRequest::Pull(l) => {
                        ureq::get(format!("{}{}?lang={}", url, l.api_key, crate::i18n::lang()).as_str())
                            .set(auth_header, l.header()?.as_str())
                            .call()
                            .map_err(|e| TR.t7ea("license", "err7", &["{call}", "{err}"], &[&mn, &e.to_string()]))?
                    }
                };
                let st = response.status();
                let r = response.into_string()
                    .map_err(|e| TR.t7ea("license", "err8", &["{call}", "{err}"], &[&mn, &e.to_string()]))?;
                trace!(log, "response [{}] from {} at {}\n{}", st, method, url, r);
                r
            }
            Some(callback) => {
                callback(&self)
            },
        };

        match self {
            ApiRequest::Create(_l) => {
                let acr: ApiResponse = serde_json::from_str(response.as_str())
                    .map_err(|e| TR.t7ea("license", "err8", &["{call}", "{err}"], &[&mn, &e.to_string()]))?;
                if acr.success {
                    let lcr: LicenseCreateResponse = serde_json::from_str(response.as_str())
                        .map_err(|e| TR.t7ea("license", "err8", &["{call}", "{err}"], &[&mn, &e.to_string()]))?;
                    info!(log, "{}", lcr.result);
                    Ok(())
                } else { Err(as_err(&acr.errors, &acr.messages)) }
            }
            ApiRequest::Update(l, _) => l.save(file, response, log),
            ApiRequest::Pull(l) => l.save(file, response, log),
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
    /// use only on create request, on update ignore and will use license DB field
    pub encrypt: bool,
    pub email: String,
    pub hosts: Vec<String>,
    pub db_names: Vec<String>,
    /// optional user lang <- Там скорее всего язык будет передаваться в заголовке и обрабатываться Request Guard-ом
    pub lang: Option<String>,
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
        email: String,
        cfg: &ConfigMap,
        db_host: &String,
        logdb_host: &String,
        db_name: &String,
        logdb_name: &String,
    ) -> Result<Self, String> {
        let encrypt = cfg.get_bool("encrypt", true);
        let mut hosts = Vec::new();
        append_name(db_host, true, &mut hosts);
        append_name(logdb_host, true, &mut hosts);
        let mut db_names = Vec::new();
        append_name(db_name, false, &mut db_names);
        append_name(logdb_name, false, &mut db_names);

        Ok(CreateUpdateRequest {
            personal: email.find("mail").unwrap_or(0) > 0,
            encrypt,
            email,
            hosts,
            db_names,
            lang: Some(crate::i18n::lang())
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
fn clean_list(list: &Vec<String>, public_key: &RSAPublicKey) -> Result<Vec<String>, String> {
    let mut res = Vec::new();
    for item in list {
        let encoded = Connection::encode(item, &public_key)?;
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

#[derive(Deserialize)]
#[allow(dead_code)]
struct LicenseCreateResponse {
    result: String,
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
    /// license name OR Created by {email}
    pub issued_to: String,
    /// email empty in evaluation.license
    #[serde(skip_serializing_if = "String::is_empty")]
    pub email: String,     //"@gmail.com"
    pub notice: String,    //"это поле свободно редактируется"
    /// set only in UI
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub connections: Vec<Connection>,
    #[serde(skip)]
    pub is_conn_encrypt: bool, // connections содержит хеши имен (len>20 & starts ~)
    #[serde(with = "rfc3339")]
    pub api_key_valid_until: LicenseTime, //"2021-12-12"
    #[serde(skip_serializing_if = "String::is_empty")]
    pub api_key: String, // "<jwt token 300-500char>"
    /// package name
    pub license_type: String,
    #[serde(skip)]
    pub version: u8,
    pub license: String, //"<signature_see_bellow -- это будет строчка 300-500 символов>"
    /// will be generated by openssl if connections are not encrypted AND 'cpk' not set
    /// on client: always set to RSAPrivateKey
    /// on server: if len > 1024, than RSAPrivateKey, else RSAPublicKey
    #[serde(skip_serializing_if = "String::is_empty")]
    pub key: String, //"<this client  RSA private key 40 строк х64char>"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    #[serde(with = "local_date_format")]
    pub valid_until: LicenseDate,
    /// RdbmState(UCB) + translatable
    pub state: String,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub hosts: BTreeSet<String>,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub db_names: BTreeSet<String>,
}

impl Connection {
    pub const UNPAID: u8 = 4;

    #[allow(unused, dead_code)]
    #[inline]
    /// digest stings (prepare for encryption) and encrypt by public key separately
    pub fn new(input: LicenseDate, code: u8) -> Self {
        Connection {
            valid_until: input,
            state: format!("{}: {}", code, translate_state(code)),
            hosts: Default::default(),
            db_names: Default::default(),
        }
    }

    #[inline]
    /// digest stings (prepare for encryption) and encrypt by public key separately
    pub(crate) fn encrypt_all(&self, public_key: &RSAPublicKey) -> Result<Vec<Vec<u8>>, String> {
        let mut encoded = Vec::new();
        for d in &self.db_names {
            encoded.push(Connection::hash_md5(d, public_key)?);
        }
        for h in &self.hosts {
            encoded.push(Connection::hash_md5(h, public_key)?);
        }
        encoded.sort();
        Ok(encoded)
    }

    #[inline]
    fn hash_md5(input: &String, public_key: &RSAPublicKey) -> Result<Vec<u8>, String> {
        if is_encrypted(input) {
            Ok(base64::decode(&input[1..]).map_err(|e| e.to_string())?)
        } else {
            let mut hasher = crypto::md5::Md5::new();
            hasher.input_str(input.as_str());
            hasher.input(public_key.n().to_bytes_be().as_ref());

            // let mut output = [0; 32]; // 256 is 2 bytes
            let mut output = [0; 16]; // 256 is 2 bytes
            hasher.result(&mut output);

            Ok(output.to_vec())
        }
    }

    /// Base64 MD5 HASH
    #[inline]
    pub fn encode(input: &String, public_key: &RSAPublicKey) -> Result<String, String> {
        if is_encrypted(input) || (input.len() == 1 && input.as_bytes()[0] == b'*') {
            Ok(input.clone())
        } else {
            let mut res = base64::encode(Connection::hash_md5(input, &public_key)?);
            res.insert(0, '~');
            Ok(res)
        }
    }

    /// Base64 RSAPublicKey encrypt
    #[inline]
    pub fn encrypt(input: &String, public_key: &RSAPublicKey) -> Result<String, String> {
        if input.len() > u8::MAX as usize || (input.len() == 1 && input.as_bytes()[0] == b'*') {
            Ok(input.clone())
        } else {
            let mut res = base64::encode(
                public_key.encrypt(&mut thread_rng(), PKCS1v15Encrypt, input.as_bytes())
                  .map_err(|e| e.to_string())?);
            res.insert(0, '~');
            Ok(res)
        }
    }

    #[inline]
    pub fn decrypt(input: &String, private_key: &RSAPrivateKey) -> Result<String, String> {
        if input.len() < u8::MAX as usize {
            Err(format!("Can not decode input of {} size", input.len()))
        } else if input.as_bytes()[0] != b'~' {
            Err(format!("The input '{}...' not supported to be decoded", input[0..8].to_string()))
        } else {
            let input = base64::decode(input[1..].to_string())
                .map_err(|e| format!("base64 decode error during decrypt: {}", e))?;
            let res = private_key.decrypt(PKCS1v15Encrypt, &input)
                .map_err(|e| format!("decrypt failed with error: {}", e))?;
            Ok(String::from_utf8_lossy(&res.as_ref()).to_string())
        }
    }

    #[inline]
    fn state_code(&self) -> u8 {
        if self.state.len() > 0 {
            let first =self.state.as_bytes()[0];
            let from = if first == b'"' || first == b'\'' { 1 } else {0};
            match self.state[from..1].parse::<u8>() {
                Ok(v) => v,
                Err(_) => 0,
            }
        } else {
            0
        }
    }

}

#[inline]
fn translate_state(code: u8) -> String {
    TR.t7e(&"connections".to_string(), &format!("state{}", code))
}

#[inline]
pub fn translate_state_sys(code: u8) -> String {
    TR.sys_code(&"connections".to_string(), &format!("state{}", code))
}

#[inline]
pub fn is_encrypted(input: &String) -> bool {
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
            description: "".to_string(),
            connections: vec![],
            is_conn_encrypt: false,
            api_key_valid_until: Utc::now().naive_utc(),
            api_key: "".to_string(),
            license_type: "".to_string(),
            version: 0,
            license: "".to_string(),
            key: "".to_string(),
        }
    }

    #[inline]
    pub fn pubk(&self) -> Result<RSAPublicKey, String> {
        Ok(if self.key.len() > 1024 {
                str2rsa(&self.key)?.to_public_key()
            } else {
                str2pub(&self.key)?
            }
        )
    }
    // License

    #[inline]
    pub fn digest(&self) -> Result<Vec<u8>, String> {
        let public_key = self.pubk();
        debug_assert!(matches!(SIGN_SH, Some(SHA2_512))); // keep two next lines in sync
        #[cfg(debug_assertions)] let mut cr = "digest.c:".to_string();
        let mut hasher = crypto::sha2::Sha512::new();
        hasher.input_str(self.issued_to.as_str());
        hasher.input_str(self.email.as_str());
        hasher.input_str(self.license_type.as_str());

        if self.license_type.rfind("_v").is_none() {
            let lv = format!("_v{}", self.version);
            hasher.input_str(lv.as_str());
        }
        for c in &self.connections {
            hasher.input(&c.state_code().to_be_bytes());
            hasher.input(&c.valid_until.year().to_be_bytes());
            hasher.input(&c.valid_until.num_days_from_ce().to_be_bytes());
            #[cfg(debug_assertions)] cr.push_str(c.valid_until.to_string().as_str());
            if c.hosts.len() > 0 || c.db_names.len() > 0 {
                // evaluation license does not have a key
                match &public_key {
                    Ok(p) => {
                        for e in &c.encrypt_all(&p)? {
                            #[cfg(debug_assertions)] crc::a(&mut cr, e.as_ref());
                            hasher.input(e.as_ref());
                        }
                    }
                    Err(e) => { return Err(e.to_owned()); }
                }
            }
        }

        let mut output = [0; 64]; // An 516 is 64 bytes
        hasher.result(&mut output);

        #[cfg(debug_assertions)] {
            cr.push_str(".result");
            crc::a(&mut cr, output.as_ref());
            println!("{}", cr);
        }

        Ok(output.to_vec())
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
    pub fn new(input: String, file: &str) -> Result<Self, String> {
        match YamlLoader::load_from_str(input.as_str()) {
            Ok(y) => {
                if y.len() > 0 {
                    let y = &y[0];
                    let mut is_conn_encrypt = false;
                    let mut connects = Vec::new();
                    if let Some(cv) = y["connections"].as_vec() {
                        for c in cv {
                            let (hosts, encr) = to_list(c, "hosts");
                            is_conn_encrypt = is_conn_encrypt || encr;
                            let (db_names, encr) =  to_list(c, "db_names");
                            is_conn_encrypt = is_conn_encrypt || encr;
                            connects.push(Connection {
                                valid_until: to_date(c, "valid_until")?,
                                state: yaml_as_str(c, "state"),
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
                        description: yaml_as_str(y, "description"),
                        connections: connects,
                        is_conn_encrypt,
                        api_key_valid_until: to_time(y, "api_key_valid_until")?,
                        api_key: yaml_as_str(y, "api_key"),
                        license: yaml_as_str(y, "license"),
                        key: yaml_as_str(y, "key"),
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
        for c in &self.connections {
            count += c.db_names.len() + c.hosts.len();
        }
        count
    }

    #[inline]
    /// validate license integrity
    pub fn verify(&self) -> Result<(), String> {
        match base64::decode(&self.license) {
            Ok(sign) => {
                let pubk = self.corp_pub_key(self.version as usize)?;
                let d= self.digest()?;
                #[cfg(debug_assertions)]
                crc::debug("verify_signd", &d, &sign, &pubk);
                match pubk.verify(
                    PKCS1v15Sign { hash: SIGN_SH },
                    &d,
                    &sign,
                ) {
                    Ok(_) => Ok(()),
                    Err(e) => {
                        #[cfg(debug_assertions)]
                        println!("Verify failed: {:?} sign: {}...", &self.connections,
                            if self.license.len() > 16 {&self.license[..16]} else {&self.license});
                        Err(format!("license: {}", e))
                    },
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
            let version = max(MIN_VER, version);
            let p = License::as_vers(&PKS[version-1 % maxv][..1]) as usize + 1;
            let version = version - 1 + (p % version);
            #[cfg(debug_assertions)]
            println!("corp_pub_key: version {}...", version);
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
        for c in &self.connections {
            if c.hosts.len() == 0 && c.db_names.len() == 0 {
                return true;
            }
        }
        false
    }

    #[inline]
    #[allow(dead_code)]
    pub fn is_appendable(&self, valid: &LicenseDate) -> bool {
        for c in &self.connections {
            if &c.valid_until == valid {
                return true;
            }
        }
        false
    }
    // License

    /// opposite to clean
    #[inline]
    fn patch(mut self, original: &License) -> Result<Self, String> {
        self.notice = original.notice.clone();
        self.key = original.key.clone();
        // #[cfg(debug_assertions)]
        // println!("patching {:?}", &self.connections);
        let key = str2rsa(&self.key)?;

        for c in self.connections.iter_mut() {
            let cs = c.db_names.len() + c.hosts.len();
            let mut dest = BTreeSet::new();
            swap(&mut c.hosts,  &mut dest);
            let _ = patch_set(dest, &mut c.hosts, &key);

            let mut dest = BTreeSet::new();
            swap(&mut c.db_names,  &mut dest);
            let _ = patch_set(dest, &mut c.db_names, &key);

            // append [state_code] with readable state (and/or) translation
            let code =  c.state_code();
            if code > 0 {
                c.state = format!("{}: {}", code, translate_state(code));
            }
            debug_assert_eq!(cs, c.db_names.len() + c.hosts.len());
        }

        #[cfg(debug_assertions)]
        println!("patched {} {:?} {}(#{})", self.api_key, &self.connections, self.license_type, self.version);
        Ok(self)
    }
    // License

    // #[inline]
    // fn is_free(&self, host_a: &String, host_b: &String, db_a: &String, db_b: &String) -> Option<LicenseDate> {
    //
    //     is_free_host(host_a)
    //     is_free_db(db_a)
    // }

    #[inline]
    fn find(&self, host_a: &String, host_b: &String, db_a: &String, db_b: &String) -> Option<LicenseDate> {
        let mut fnd: Option<LicenseDate> = None;
        let (host_a, host_b) = if host_a.len() == 0 && host_b.len() > 0 {
            (host_b, host_a)
        } else {
            (host_a, host_b)
        };
        let (db_a, db_b) = if db_a.len() == 0 && db_b.len() > 0 {
            (db_b, db_a)
        } else {
            (db_a, db_b)
        };
        if host_a.len() > 0 && db_a.len() > 0 {
            let star = STAR.to_string();
            for c in &self.connections {
                if c.state_code() != Connection::UNPAID
                    && ((c.hosts.contains(&star)
                    || ((c.hosts.contains(host_a) || is_free_host(host_a))
                    && (host_b.len() == 0 || is_free_host(host_b) || c.hosts.contains(host_b))))
                    && (c.db_names.contains(&star)
                    || ((c.db_names.contains(db_a) || is_free_db(db_a))
                    && (db_b.len() == 0 || is_free_db(db_b) || c.db_names.contains(db_b))))
                ) {
                    if {
                        if let Some(f) = &fnd {
                            c.valid_until > *f
                        } else { true }
                    } {
                        fnd = Some(c.valid_until.clone());
                    }
                }
            }
        }
        // #[cfg(debug_assertions)]
        // println!("Find {},{},{},{} in {:?} = {:?}", host_a, host_b, db_a,db_b, self.connections, fnd);
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
                PKCS1v15Sign { hash: HEADER_SH },
                // PaddingScheme::new_pss::<Sha1, _>(thread_rng()),
                &d,
            ) {
                Ok(s) => {
                    crc::debug("do_header", &d, &s,  &rsa.to_public_key());
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
        debug_assert!(matches!(HEADER_SH, Some(SHA2_256))); // keep two lines in sync
        let mut hasher = crypto::sha2::Sha256::new();
        hasher.input_str(self.api_key.as_str());
        hasher.input_str(self.email.as_str());

        let mut output = [0; 32];
        hasher.result(&mut output);

        output.to_vec()
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
                crc::debug("is_header", &d, &sign, &pubk);
                pubk.verify(PKCS1v15Sign { hash: HEADER_SH },
                    &d, &sign)
                    .map_err(|e| format!("invalid auth header: {}", e))
            }
            Err(e) => Err(format!("reading header [{}]: {}", auth.len(), e)),
        }
    }
    // License

    #[inline]
    fn save(&self, file: &String, response: String, log: &Logger) -> Result<(), String> {
        let lr: LicenseResponse = serde_json::from_str(response.as_str())
            .map_err(|e| TR.t7ea("license", "save_err1", &["{call}", "{err}"], &[&response, &e.to_string()]))?;
        match lr.result {
            None => Err(as_err(&lr.errors, &lr.messages)),
            Some(lr) => {
                let lr = lr.convert().patch(self)?;
                let (file, resp) = match lr.verify() {
                    Ok(_) => (file.clone(), Ok(())),
                    Err(e) => (format!("{}.err", file), Err(e))
                };
                fs::write(&file, serde_yaml::to_string(&lr)
                    .map_err(|e| TR.t7ea("license", "save_err2", &["{err}"], &[&e.to_string()]))?)
                    .map_err(|e| TR.t7ea("license", "save_err3", &["{file}", "{err}"], &[&file, &e.to_string()]))?;
                info!(log, "{}", TR.t7ea("license", match &resp { Ok(_) => "call_ok", Err(_) => "call_notok"}, &["{file}"], &[&file]));
                resp
            }
        }

    }
    // License

}

#[inline]
fn is_free_host(host: &String) -> bool {
    host.as_str() == LH || is_ip(host)
}

#[inline]
fn is_free_db(db: &String) -> bool {
    db.as_str() == PG || db == &env::var("USER").unwrap_or("".into())
}


const HEADER_SH: Option<rsa::hash::Hash> = Some(SHA2_256);
pub const SIGN_SH: Option<rsa::hash::Hash> = Some(SHA2_512);

fn patch_set(src: BTreeSet<String>, dest: &mut BTreeSet<String>, key: &RSAPrivateKey)->Result<(), String> {
    let ol = src.len();
    for name in src {
        let decoded = if name.len()> u8::MAX as usize {
            Connection::decrypt(&name, &key)?
        } else {
            name.clone()
        };
        #[cfg(debug_assertions)]
        println!("patching[{}/{}] {}~>{}", dest.len() + 1, ol, name[..std::cmp::min(8, name.len())].to_string(), decoded);
        dest.insert(decoded);
    }
    debug_assert_eq!(ol, dest.len());
    Ok(())
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

#[allow(dead_code, unused, warnings)]
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
    #[allow(unused, warnings)]
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
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: Option<String>,
    pub connections: Vec<JopConnection>,
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
    pub state: String,
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
            state: self.state,
            hosts,
            db_names
        }, is_conn_encrypt)
    }
}

impl JopLicense {
    fn convert(self) -> License {
        let mut is_conn_encrypt = false;
        let mut connects = Vec::new();
        for c in self.connections {
            let (con, encr) = c.convert(is_conn_encrypt);
            is_conn_encrypt = is_conn_encrypt || encr;
            connects.push(con);
        }
        let (_, version) = License::parse_version(self.license_type.clone());
        License {
            issued_to: self.issued_to,
            email: self.email.unwrap_or("".to_string()),
            notice: "".to_string(),
            description: self.description.unwrap_or("".to_string()),
            connections: connects,
            is_conn_encrypt,
            api_key_valid_until: self.api_key_valid_until,
            api_key: self.api_key.unwrap_or("".to_string()),
            license_type: self.license_type,
            version,
            license: self.license,
            key: self.key.unwrap_or("".to_string()),
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

const KEY_ERR_MSG: &str = "Stored key cant get converted to RSA PKCS1.";

#[inline]
pub fn str2rsa(input: &str) -> Result<RSAPrivateKey, String> {
    if input.len() > 1000 {
        match base64::decode(input) {
            Ok(der_bytes) => {
                    match RSAPrivateKey::from_pkcs1(&der_bytes) {
                        Ok(key) => Ok(key),
                        Err(e) => Err(format!("{} {:?}", KEY_ERR_MSG, e)),
                    }
            }
            Err(e) => Err(format!("{} Base64 error: {}", KEY_ERR_MSG, e)),
        }
    } else {
        Err(format!("{} The size {}, is too small", KEY_ERR_MSG, input.len()))
    }
}

#[cfg(test)]
#[allow(unused)]
mod tests {
    use std::thread::sleep;
    use std::time::Duration;

    use super::*;
    use chrono::{Local, DateTime};
    use uuid::Uuid;

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
    fn test_sign_lic() -> Result<(), String> {
        let lf = "tests/signed_test_rdbm.license";
        if Path::new(lf).is_file() {
            let l = License::load(lf)?;
            l.verify()?;
        }
        Ok(())
    }

    #[test]
    fn test_header_lic() -> Result<(), String> {
        let lf = "tests/signed_test_rdbm.license";
        if Path::new(lf).is_file() {
            let mut l = License::load(lf)?;
            let h = l.header()?;
            assert!(h.len() > 0);
            l.key = pub2str(&str2rsa(&l.key)?.to_public_key());
            l.is_header(&h)?;
        }
        Ok(())
    }

    #[test]
    fn patch_test() -> Result<(), String> {
        let mut l = License::blank();
        let now: LicenseDate = Utc::today().naive_utc();
        let pk = str2rsa(TEST_PK).map_err(|e| e.to_string())?;
        let pp = pub2str(&pk.to_public_key());
        println!("{}", pp.len());
        // l.cpk = Some(pp);
        l.key = TEST_PK.to_string();
        let cr = CreateUpdateRequest {
            personal: false,
            encrypt: false,
            email: "em".to_string(),
            hosts: vec!["host".into(), "host1".into() ],
            db_names: vec!["db".into(), "db1".into() ],
            lang: None
        };

        let mut lr = License::blank();
        lr.connections.push(Connection{
            valid_until: now,
            state: "3".to_string(),
            hosts: Default::default(),
            db_names: Default::default()
        });
        for x in &cr.hosts {
            lr.connections[0].hosts.insert(Connection::encrypt(x, &pk.to_public_key())?);
        }
        for x in &cr.db_names {
            lr.connections[0].db_names.insert(Connection::encrypt(x, &pk.to_public_key())?);
        }

        assert_ne!(&cr.hosts[0], lr.connections[0].hosts.iter().next().unwrap());

        let lr = lr.patch(&l)?;

        assert_eq!(&cr.hosts[0], lr.connections[0].hosts.iter().next().unwrap());
        assert_eq!(&cr.db_names[0], lr.connections[0].db_names.iter().next().unwrap());

        Ok(())
    }

    static TEST_PK:&'static str = include_str!("../tests/test.key");

    #[test]
    fn encode_test() -> Result<(), String> {
        let pk = str2rsa(TEST_PK).map_err(|e| e.to_string())?;

        let src = "зеhostsблин".to_string();
        let e = Connection::encrypt(&src, &pk.to_public_key())?;

        assert_eq!(Connection::decrypt(&e, &pk)?, src);

        Ok(())
    }

    #[test]
    fn digest_test() -> Result<(), String> {
        let mut l = License::blank();
        let now: LicenseDate = Utc::today().naive_utc();
        l.license_type = "Std".to_string();
        l.key = TEST_PK.to_string();
        let c = Connection{
            valid_until: now,
            state: "".to_string(),
            hosts: Default::default(),
            db_names: Default::default()
        };
        l.connections.push(c.clone());
        l.connections.push(c);
        for i in 10..23 {
            let x = i % 2;
            l.connections[x].db_names.insert(format!("db_name_{}_{}", i, i));
            l.connections[x].hosts.insert(format!("host_{}_{}", i, i));
        }
        // let cc = l.cnt();
        let mut l_original_request = l.clone();
        let d = l.digest()?;
        l.key = pub2str(&str2rsa(&l.key)?.to_public_key());
        assert_eq!(l.digest()?, d);

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
        test_find_n(true, true);
        test_find_n(false, true);
        test_find_n(true, false);
        test_find_n(false, false);
    }

    #[inline]
    fn uuid() -> String {
        let x = Uuid::new_v4();
        x.to_hyphenated().to_string()[..8].into()
    }

    #[inline]
    fn gen_test_name(start: bool) -> (String, String) {
       let name = uuid();
       if start { ("*".to_string(), name) } else {(name.clone(), name)}
    }

    #[inline]
    fn test_find_n(star_host: bool, star_db: bool) {
        let mut l = License::blank();
        l.connections.push(Connection::new(Utc::today().naive_utc() + chrono::Duration::days(1), 0));
        let e = "".to_string();
        let c = Utc::today().naive_utc();
        assert!(l.find(&e, &e, &e,&e).is_none());
        let user = env::var("USER").unwrap_or("".to_string());
        let h = "localhost".to_string();
        let p = "postgres".to_string();
        let i = "127.0.0.1".to_string();
        test_find_assert_gt(&l, &c, &h, &user);
        test_find_assert_gt(&l, &c, &i, &user);
        test_find_assert_gt(&l, &c, &h, &p);
        test_find_assert_gt(&l, &c, &i, &p);

        let (append_host, sample_host) = gen_test_name(star_host);
        let (append_db, sample_db) = gen_test_name(star_db);

        l.connections[0].hosts.insert(append_host.clone());
        l.connections[0].db_names.insert(append_db.clone());

        test_find_assert_gt(&l, &c, &sample_host, &sample_db);
        test_find_assert_gt(&l, &c, &sample_host, &user);
        test_find_assert_gt(&l, &c, &sample_host, &p);
        test_find_assert_gt(&l, &c, &h, &sample_db);
        test_find_assert_gt(&l, &c, &i, &sample_db);

        l.connections.push(Connection::new(Utc::today().naive_utc() - chrono::Duration::days(10), 0));
        l.connections[1].hosts.insert(append_host.clone());
        l.connections[1].db_names.insert(append_db.clone());

        test_find_assert_gt(&l, &c, &sample_host, &sample_db);
        test_find_assert_gt(&l, &c, &sample_host, &user);
        test_find_assert_gt(&l, &c, &sample_host, &p);
        test_find_assert_gt(&l, &c, &h, &sample_db);
        test_find_assert_gt(&l, &c, &i, &sample_db);

        let c = Utc::today().naive_utc() - chrono::Duration::days(100);
        l.connections.remove(0);

        test_find_assert_gt(&l, &c, &sample_host, &sample_db);
        test_find_assert_gt(&l, &c, &sample_host, &user);
        test_find_assert_gt(&l, &c, &sample_host, &p);
        test_find_assert_gt(&l, &c, &h, &sample_db);
        test_find_assert_gt(&l, &c, &i, &sample_db);
    }

    fn test_find_assert_gt(l: &License, compare_to: &NaiveDate, host: &String, db: &String) {
        let e = "".to_string();
        assert!(l.find(&e, &e, &e, &e).is_none());
        assert!(l.find(host, &e, &e, &e).is_none());
        assert!(l.find(&e, &e, &db, &e).is_none());
        assert!(l.find(host, &e, &db, &e).unwrap() > *compare_to);
        assert!(l.find(host, &e, &e, &db).unwrap() > *compare_to);
        assert!(l.find(host, &e, &db, &db).unwrap() > *compare_to);
        assert!(l.find(&e, &host, &db, &e).unwrap() > *compare_to);
        assert!(l.find(&e, &host, &e, &db).unwrap() > *compare_to);
        assert!(l.find(&e, &host, &db, &db).unwrap() > *compare_to);
        assert!(l.find(&host, &host, &db, &e).unwrap() > *compare_to);
        assert!(l.find(&host, &host, &e, &db).unwrap() > *compare_to);
        assert!(l.find(&host, &host, &db, &db).unwrap() > *compare_to);
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
        l.connections.push(Connection::new(Utc::today().naive_utc() + chrono::Duration::days(1), 0));
        l.connections[0].db_names.insert("*".into());
        //only db name
        assert!(l.find(&i, &h, &zzz, &p).is_some());
        assert!(l.find(&i, &h, &user, &zzz).is_some());
        assert!(l.find(&zzz, &h, &zzz, &p).is_none());
        assert!(l.find(&i, &zzz, &p, &zzz).is_none());

        // expiring license
        l.connections[0].valid_until = Utc::today().naive_utc() - chrono::Duration::days(1);
        l.connections.push(Connection::new(Utc::today().naive_utc() + chrono::Duration::days(1), 0));
        // expired license
        assert!(l.find(&i, &h, &zzz, &p).unwrap() < Utc::today().naive_utc());
        assert!(l.find(&i, &h, &user, &zzz).unwrap() < Utc::today().naive_utc());
        assert!(l.find(&zzz, &h, &zzz, &p).is_none());
        assert!(l.find(&i, &zzz, &p, &zzz).is_none());
        // eval license
        assert!(l.find(&i, &h, &user, &e).unwrap() > Utc::today().naive_utc());
        l.connections[1].hosts.insert("*".into());
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
    fn test_tr() {
        assert_eq!(translate_state(2), "New".to_string());
        assert_eq!(translate_state_sys(2), "new".to_string());
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
    "connections": [{
        "valid_until": "2022-07-22", "state": ""
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
    "connections": [
      {
        "db_names": [
          "~OSKI1KjFT4LZKgmQjIFfiq8Ywsk="
        ],
        "valid_until": "2022-07-22", "state": ""
      },
      {
        "hosts": [
          "~x3nzzDJtyr7C3zP18njCu0Njbww="
        ],
        "valid_until": "2022-07-22", "state": ""
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

