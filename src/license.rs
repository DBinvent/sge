#[allow(unused)]

use std::collections::BTreeSet;
use std::{fs, env};
use std::io::Write;
use std::path::Path;

use chrono::{DateTime, FixedOffset, Utc};
use crypto_hash::Algorithm;
use rand::thread_rng;
use rsa::{BigUint, PaddingScheme, PublicKey, PublicKeyParts, RSAPrivateKey, RSAPublicKey};
use serde::{Serialize, Deserialize};
use sha1::{Digest, Sha1};
use slog::Logger;
use yaml_rust::{Yaml, YamlLoader};
use crate::ConfigMap;
use std::collections::HashMap;


const PK: &str = "tYV/+g8q0/K0DhWxcrTesII0/ludUrOxDsHTopTcV5FmyXzCrK75z3ARiK67pHa5r5sQCkstK0JKs2/izmp2sjfsm/lLSmLvViBgbzd96bQYZe6Z3zBhyLHa4xVNe5o+VeWX7tOI5EenG0JPkvfMGijS4AHbQs33SZ1z8UKRg8ES4Ab/YK3d/ddfJ8Fs+fvDyuoO+NK10te/wdsvZgsK87v36hLoZi7ZwKSkdzH0dxC6JqM/YYyA72fMFtoRw6s3gT8rdjuLHPuyIh7OC5W7ufH9Fe0r10tS0KeUsNo77ZM82HdknWbSsznl3RyWkj5C0dY9ez+TK3xSot4Gfj/a5tFUyLIJMP+QlPTQZlXO+04ClOJDvJBfd8A+x0i5DqQgbwV3rBSzAi30086pOTxmlZvfT7Zze1Q6KJZhCukNcLDLxqnDm8gV/i7/i3f5EmsIbOvKKBHdNi1QDDqmgG787BXxW9a34Po9APxT4nALBWPqLTMs1cx6MpOxJBJelKr9AQAB";

const INV_LL: &str = "Please check https://rdbm.dbinvent.com/license See ./rdbm help license for more details.";
#[allow(dead_code)]
const LL: &str = "https://rdbm.dbinvent.com/license";

/// on license action
pub fn license(cfg: ConfigMap, log: &Logger) -> Result<(), String> {
    let email = cfg.get("email");
    let license_file = cfg.get("license_file");

    let mut subsr: BTreeSet<String> = BTreeSet::new();
    cfg.get("subscribe").split(",").for_each(|s| { subsr.insert(s.into()); });
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
            Ok(l) => {
                match l.verify() {
                    Ok(_) => {
                        info!(log, "License [{}] issued to: {} {}{}", license_file, l.issued_to, l.email,
                            if do_subscribe {", will try to subscribe if necessary"} else {""});

                        if !do_subscribe { // TODO check expire
                            request(license_file.as_str(), email.as_str(), Some(l), do_subscribe, log)
                        } else if l.find(&db_host,&logdb_host, &db_name, &logdb_name)  {
                            info!(log, "Good to use for your connection! Thank you! ");
                            Ok(())
                        } else if email.len() > 0 && do_subscribe{
                            request(license_file.as_str(), email.as_str(), Some(l), do_subscribe, log)
                        } else {
                            Err(format!("The license loaded from file [{}] is not valid: {}. Set '--email' and use '--subscribe' to request", license_file, INV_LL))
                        }
                    }
                    Err(e) => Err(format!("The license loaded from file [{}] is not valid: {}. {}", license_file, e, INV_LL))
                }
            }
            Err(e) => Err(format!("The license loaded from file [{}] is not valid: {}. {}", license_file, e, INV_LL))
        }
    } else if email.len() > 0 && do_subscribe{
        request(license_file.as_str(), email.as_str(), None, do_subscribe, log)
    } else {
        Err(format!("No '--email' nor '--license_file' set. Please check ./rdbm help license for more details."))
    }
}

#[inline]
fn request(file: &str, email: &str, lic: Option<License>, subscribe: bool, log: &Logger) -> Result<(), String>  {
    let url = "http://localhost:8000/api/license"; // LL
    let auth_header = "X-Header";
    let mut encm = HashMap::new();
    let (method, response) = match &lic {
        None => ("POST", ureq::post(url).set(auth_header, "newbie")
                .send_string(email).map_err(|e| format!("creating license: {}", e))?),
        Some(l) => {
            let request = l.clean(&mut encm)?;
            if subscribe {
                ("PUT", ureq::put(url)
                    .set(auth_header, l.header()?.as_str())
                    .send_string(request.as_str())
                    .map_err(|e| format!("updating license: {}", e))?)
            } else {
                ("GET", ureq::get(format!("{}/{}", url, l.api_key).as_str())
                    .set(auth_header, l.header()?.as_str())
                    .call()
                    .map_err(|e| format!("upcheking license: {}", e))?)
            }
        }
        };
    debug!(log, "response [{}] from {} {}", response.status(), method, url);
    let lr: LicenseResponse = serde_json::from_str(response.into_string()
        .map_err(|e| format!("getting response: {}", e))?.as_str())
            .map_err(|e| format!("converting response: {}", e))?;
    let mut l = lr.result;
    debug!(log, "loaded: {:?} ", l);
    if match lic {
        None => true,
        Some(lr) => {
            if l.patch(lr, &encm) {
                l.verify()?;
                true
            } else { false}
        }
    } {
        // TODO remove after ubc complete
        let file = format!("x.{}", file);
        debug!(log, "saving to {}", file);
        fs::write(file.clone(), serde_yaml::to_string(&l)
            .map_err(|e| format!("creating license: {}", e))?)
                .map_err(|e| format!("writing to [{}] {}", file, e))
    } else {
        Ok(())
    }
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct LicenseResponse {
    result: License,
    success: bool,
}

#[cfg(debug_assertions)]
pub fn is_ok(_cfg: &ConfigMap, _log: &Logger) -> Result<bool, String> {
    Ok(true)
}

#[cfg(not(debug_assertions))]
#[inline]
/// on migrate action
pub fn is_ok(cfg: &ConfigMap, _log: &Logger) -> Result<bool, String> {
    match cfg.get_opt("license_file") {
        None => Err("--license_file is not set".into()),
        Some(lf) => {
            let l = License::load(lf.as_str())?;
            l.verify()?;
            Ok(l.find(&cfg.get("db_host"),
                            &cfg.get("logdb_host"),
                            &cfg.get("db_name"),
                            &cfg.get("logdb_name") ))
        }
    }
}

type LicenseDate = chrono::DateTime<FixedOffset>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct License {
    pub issued_to: String, //"Evaluate"
    pub email: String,     //"@gmail.com"
    pub notice: String,    //"это поле свободно редактируется"
    pub connects: Vec<Connection>,
    #[serde(with = "local_date_format")]
    pub api_key_valid_until: LicenseDate, //"2021-12-12"
    pub api_key: String,   // "<jwt token 300-500char>"
    license: String,              //"<signature_see_bellow -- это будет строчка 300-500 символов>"
    crc: String, // debug
    /// will be generated by openssl if connections are not encrypted AND 'cpk' not set
    pub(crate) key: String,       //"<this client  RSA private key 40 строк х64char>"
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
    pub(crate) fn encrypt(input: &String, public_key: &RSAPublicKey) -> Result<Vec<u8>, String> {
		let mut hasher = crypto_hash::Hasher::new(Algorithm::SHA256);
        let _ = hasher.write(input.as_bytes());
        let _ = hasher.write(&public_key.e().to_bytes_be());
        let _ = hasher.write(&public_key.n().to_bytes_be());
        let d = hasher.finish();
        Ok(Sha1::digest(&d).to_vec())
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
            api_key_valid_until: Utc::now().into(),
            api_key: "".to_string(),
            license: "".to_string(),
            crc: "".to_string(),
            key: "".to_string(),
            cpk: None
        }
    }

    #[inline]
    fn pubk(&self) -> Result<RSAPublicKey, String> {
        Ok(match &self.cpk {
            None => str2rsa(&self.key)?.to_public_key(),
            Some(s) => str2pub(s)?
        })
    }

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
            let _ = hasher.write(c.valid_until.to_rfc3339().as_bytes());
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



    #[inline]
    #[allow(dead_code)]
    pub fn sign(&mut self, s: Vec<u8>, rsa: Option<String>, crc: String) {
        if let Some(rk) = rsa {
            self.key = rk;
        }
        self.crc = crc;
        self.license = base64::encode(s);
    }

    #[inline]
    pub(crate) fn load(file: &str) -> Result<Self, String> {
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
                        issued_to: y["issued_to"].as_str().unwrap_or("").into(),
                        email: y["email"].as_str().unwrap_or("").into(),
                        notice: y["notice"].as_str().unwrap_or("").into(),
                        connects,
                        api_key_valid_until: to_date(y, "api_key_valid_until")?,
                        api_key: y["api_key"].as_str().unwrap_or("").into(),
                        license: y["license"].as_str().unwrap_or("").into(),
                        crc: y["crc"].as_str().unwrap_or("").into(),
                        key: y["key"].as_str().unwrap_or("").into(),
                        cpk: None
                    })
                } else {
                    Err(format!("no config in {} ", file))
                }
            },
            Err(e) => Err(format!("wrong license {}: {}", file, e)),
        }
    }

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
        match base64::decode(&self.license){
            Ok(sign) => {
                let (d, crc) = self.digest()?;
                match hardcoded()?.verify(PaddingScheme::new_pss::<Sha1, _>(thread_rng()),
                    &d, &sign,
                ) {
                    Ok(_) => Ok(()),
                    Err(e) => Err(format!("license: {}, signed: {}, calculated: {}", e, self.crc, crc))
                }
            }
            Err(e) => Err(format!("reading license: {}", e))
        }
    }

    #[inline]
    #[allow(dead_code)]
    pub(crate) fn rewrite(&self, input: String, file: &str) -> Result<(), String> {
        let mut l = License::new(input, "https://rdbm.dbinvent.com/")?;
        l.key = self.key.clone();

        match serde_yaml::to_string(&self) {
            Ok(l) => {
                match fs::write(file, l) {
                    Ok(_) => Ok(()),
                    Err(e) => Err(format!("writing to [{}] {}", file, e)),
                }
            },
            Err(e) => Err(format!("writing to [{}] {}", file, e)),
        }
    }

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
                let e = base64::encode(Connection::encrypt(host, &public_key)?);
                enc.insert(e.clone(), host.clone() );
                c.hosts.insert(e);
            }
            let db_names = c.db_names.to_owned();
            c.db_names = BTreeSet::new();
            for db_name in &db_names {
                let e = base64::encode(Connection::encrypt(db_name, &public_key)?);
                enc.insert(e.clone(), db_name.clone());
                c.db_names.insert(e);
            }
        }
        serde_yaml::to_string(&lic)
            .map_err(|e| format!("creating payload of a license: {}", e))
    }

    // TODO implement revert
    /// opposite to clean
    #[inline]
    fn patch(&mut self, lic: License, enc: &HashMap<String, String>) -> bool {
        self.notice = lic.notice;
        self.key = lic.key;
        let mut self_now = Utc::now().into();
        let mut lic_now = Utc::now().into();
        for c in &lic.connects {
            if c.valid_until > lic_now { lic_now = c.valid_until.clone(); }
        }
        for c in &mut self.connects {
            if c.valid_until > self_now { self_now = c.valid_until.clone(); }
            let hosts = c.hosts.to_owned();
            c.hosts.clear();
            for host in &hosts {
                c.hosts.insert(enc.get(host).unwrap_or(host).into());
            }
            let db_names = c.db_names.to_owned();
            c.db_names.clear();
            for db_name in &db_names {
                c.db_names.insert(enc.get(db_name).unwrap_or(db_name).into());
            }
        }
        lic_now > self_now
    }

    #[inline]
    fn find(&self, host_a: &String, host_b: &String, db_a: &String, db_b: &String) -> bool {
        let user = &env::var("USER").unwrap_or("".into());
        let now: DateTime<FixedOffset> = Utc::now().into();
        for c in &self.connects {
            if c.valid_until > now {
                if ( c.hosts.contains("*") || ( (is_ip(host_a)  || c.hosts.contains(host_a)) && (is_ip(host_b) || c.hosts.contains(host_b) )))
                 && (((db_a == "" || db_a == user) && (db_b == "" || db_b == user)) ||
                 c.db_names.contains("*") || ( c.db_names.contains(db_a) && ( db_b.len() == 0 || c.db_names.contains(db_b) ))
                 ) {
                    return true
                 }
            }
        }
        false
    }

    /// generate authorization header
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn header(&self) -> Result<String, String> {
        let now: DateTime<FixedOffset> = Utc::now().into();
        if self.api_key.len() < 30 {
            Err("api_key is not set".into())
        } else if self.api_key_valid_until < now {
            Err("api_key is outdated".into())
        } else if self.key.len() < 300 {
            Err("key is not set".into())
        } else {
            let rsa = match base64::decode(&self.key) {
					Ok(der_bytes) => {
                        match RSAPrivateKey::from_pkcs1(&der_bytes) {
                            Ok(key) => key,
                            Err(e) => { return Err(format!("key is not good: {:?}", e)); }
                        }
					}
                    Err(e) => { return Err(format!("key is not good: {}", e)); }
				};

            let d = self.header_digest();
            match rsa.sign_blinded(&mut thread_rng(), PaddingScheme::new_pss::<Sha1, _>(thread_rng()), &d) {
                Ok(s) => Ok(format!("{}:{}", self.api_key, base64::encode(s))),
                Err(e) => Err(format!("key is not good for sign: {}", e))
            }
        }
    }

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
    pub(crate) fn is_header(&self, auth: &str) -> Result<(), String> {
        match base64::decode(auth) {
            Ok(sign) => {
                let d = self.header_digest();
                match self.pubk()?.verify(PaddingScheme::new_pss::<Sha1, _>(thread_rng()),
                    &d, &sign,
                ) {
                    Ok(_) => Ok(()),
                    Err(e) => Err(format!("invalid auth header: {}", e))
                }
            }
            Err(e) => Err(format!("reading header: {}", e))
        }
    }

}

#[inline]
/// check for empty string, localhost, unix socket, or IP
fn is_ip(host: &String) -> bool {
    host.len() == 0 || host.as_str() == "localhost" || host.as_bytes()[0] == b'/' ||
    match ipaddress::IPAddress::parse(host) {
        Ok(_) => true, Err(_) => false,
    }
}

mod crc {
    use std::fmt::Write;

    #[cfg(not(debug_assertions))]
    #[inline]
    pub(crate) fn a(_s: &mut String, _d: &[u8]) { }


    #[cfg(debug_assertions)]
    #[inline]
    pub(crate) fn a(s: &mut String, d: &[u8]) {
        let _ = write!(s, ".{}",
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
    if let Some(list) =  yaml[input].as_vec() {
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
        Some(s) => {
            match DateTime::parse_from_rfc3339(s) {
                Ok(d) => Ok(d),
                Err(e) => Err(format!("date parsing [{}] error: {}", s, e))
            }
        }
    }
}

mod local_date_format {
    use chrono::{DateTime, FixedOffset};
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(date: &DateTime<FixedOffset>, serializer: S, ) -> Result<S::Ok, S::Error> where S: Serializer {
        serializer.serialize_str(&date.to_rfc3339())
    }
    #[allow(dead_code)]
    pub fn deserialize<'de, D>(deserializer: D) -> Result<DateTime<FixedOffset>, D::Error> where D: Deserializer<'de> {
        DateTime::parse_from_rfc3339( String::deserialize(deserializer)?.as_str()).map_err(serde::de::Error::custom)
    }
}

/// create base64 string from RSA, use str2rsa(key).to_public_key() -> pub2str() -> str2pub()
#[inline]
#[allow(dead_code)]
pub fn pub2str(input: &RSAPublicKey) -> String {
	format!("{}{}",
		base64::encode(input.n().to_bytes_be()),
		base64::encode(input.e().to_bytes_be()),
	)
}

#[inline]
pub fn str2pub(input: &String) -> Result<RSAPublicKey, String> {
    let i = input.len();
    if i < 100 { Err(format!("malformed for RSA key [{}]", input.len())) }
    else {
        let i = i - 4;
        match strs2pub(&input[..i], &input[i..]) {
            Ok(key) => Ok(key),
            Err(e) => Err(format!("{:?}", e))
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
                    Err(e) => Err(format!("{:?}", e))
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

    #[test]
    fn test_date() {
        let one = Utc::now();
        sleep(Duration::from_millis(100));
        let two = Utc::now();
        let t: DateTime<FixedOffset> = two.into();
        assert!(one < two);
        let mut ss: BTreeSet<String> = BTreeSet::new();
        "a".split(",").for_each(|c| {ss.insert(c.into());});
        assert_eq!(ss.len(), 1);
        assert_eq!(ss.iter().next().unwrap_or(&"".to_string()), &"a".to_string());
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
        let lf = "tests/signed_test_rdbm.license";
        if Path::new(lf).is_file() {
            let l = License::load(lf)?;
            l.verify()?;
            let h = l.header()?;
            assert!(h.len() > 0);
            let i = h.find(":").unwrap();
            assert_eq!(l.api_key, h[..i]);
            l.is_header(&h[i+1..])?;

        }
        Ok(())
    }

    #[test]
    fn clean_lic() -> Result<(), String> {
        let mut l = License::blank();
        l.cpk = Some(PK.into());
        l.key = "a".into();
        let mut hosts = BTreeSet::new();
        hosts.insert("h".to_string());
        let mut db_names = BTreeSet::new();
        db_names.insert("db".to_string());
        l.connects.push(Connection {
            valid_until: Utc::now().into(), hosts, db_names
        });
        let mut enc = HashMap::new();
        let cl = l.clean(&mut enc)?;
        assert_eq!(enc.len(), 2);
        println!("{:?}", enc);

        let mut cl = License::new(cl, "test")?;
        assert_eq!(cl.connects.len(), 1);
        assert!(!cl.connects[0].hosts.contains("h"));
        assert!(!cl.connects[0].db_names.contains("db"));
        cl.patch(l, &mut enc);
        assert_eq!(cl.connects.len(), 1);
        assert!(cl.connects[0].hosts.contains("h"));
        assert!(cl.connects[0].db_names.contains("db"));
        Ok(())
    }

}