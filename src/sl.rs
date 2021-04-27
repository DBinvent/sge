#[macro_use]
extern crate slog;

use std::{env, fs};
use yaml_rust::{Yaml, YamlLoader, ScanError, YamlEmitter};
use rsa::{RSAPrivateKey, RSAPublicKey, PublicKeyParts, BigUint, PaddingScheme};
use rsa::errors::Error;
use crate::license::License;
use std::process::Command;
use rand::thread_rng;
use sha1::Sha1;

mod license;

/// ssh-keygen -t rsa -m PKCS8  -f corp.rsa
fn main() -> Result<(), String> {
    let args = env::args();
    if args.len() < 3 {
        println!("sign the license by: ./sl _.rsa _.license");
    } else {
        let mut files = Vec::new();
        env::args().for_each(|a| files.push(a));

        let mut lic = License::load(&files[2])?;
        sign(&mut lic, &load_rsa(&files[1])?);

        match serde_yaml::to_string(&lic) {
			Ok(l) => println!("{}", l),
			Err(e) => eprintln!("{}", e),
		}
    }
    Ok(())
}

#[inline]
pub fn load_rsa(filename: &str) -> Result<RSAPrivateKey, String> {
    match loadrsa(filename) {
        Ok(rsa) => Ok(rsa),
        Err(e) => Err(format!("{:?}", e))
    }
}

#[inline]
fn loadrsa(filename: &str) -> Result<RSAPrivateKey, rsa::errors::Error> {
	match fs::metadata(filename) {
		Ok(file) => match fs::read_to_string(filename) {
			Ok(file_content) => {
				let file_len = file.len();
				let der_encoded = file_content
					.lines()
					.filter(|line| !line.starts_with("-"))
					.fold(String::new(), |mut data, line| {
						data.push_str(&line);
						data
					});
				match base64::decode(&der_encoded) {
					Ok(der_bytes) => {
						if file_len > 2000 {
							RSAPrivateKey::from_pkcs8(&der_bytes)
						} else if file_len > 1000 {
							RSAPrivateKey::from_pkcs1(&der_bytes)
						} else {
							Err(rsa::errors::Error::ParseError {
								reason: format!("file is too small: {}", file_len),
							})
						}
					}
					Err(e) => Err(rsa::errors::Error::ParseError {
						reason: e.to_string(),
					}),
				}
			}
			Err(e) => Err(rsa::errors::Error::ParseError {
				reason: format!("read: {}", e),
			}),
		},
		Err(e) => Err(rsa::errors::Error::ParseError {
			reason: format!("file: {}", e),
		}),
	}
}

/// Sign all connections from user license, generate users RSA if key or cpk not set
#[inline]
pub(crate) fn sign(lic: &mut License, rsa: &RSAPrivateKey) -> Result<(), String> {
	if let None = &lic.cpk {
		if lic.key.len() < 1000 {
			lic.key = genrsa()?;
		}
	};
	let (d, crc) = lic.digest()?;
	match rsa.sign_blinded(&mut thread_rng(), PaddingScheme::new_pss::<Sha1, _>(thread_rng()), &d) {
		Ok(s) => {
			lic.sign(s, crc);
			Ok(())
		}
		Err(e) => Err(format!("sign: {}", e))
	}
}


fn genrsa() -> Result<String, String> {
    match Command::new("openssl").arg("genrsa").output()
    {
        Ok(output) => {
            if output.status.success() {
				let der_encoded = String::from_utf8_lossy(&output.stdout).to_string()
					.lines()
					.filter(|line| !line.starts_with("-"))
					.fold(String::new(), |mut data, line| {
						data.push_str(&line);
						data
					});

                Ok(der_encoded)
            } else {
                Err(format!("openssl> {}{}", String::from_utf8_lossy(&output.stderr), String::from_utf8_lossy(&output.stdout)))
            }
        }
        Err(e) => Err(format!("openssl> {}", e)),
    }

}


#[cfg(test)]
mod tests {
    use super::*;

	#[test]
    fn init_pub() {
		match license::hardcoded() {
			Ok(hkey) => {
				assert!(true);
			}
			Err(e) => {
				assert!(false, "{}", e);
			}
		}
    }

	// # [test]
    fn init_rsa() {
		match load_rsa("tests/corp.rsa") {
			Ok(key) => {
				// println!("{}", license::pub2str(&key.to_public_key()));
				match license::hardcoded() {
					Ok(hkey) => {
						assert_eq!(license::pub2str(&key.to_public_key()), license::pub2str(&hkey) );
						// assert!(true);
					}
					Err(e) => {
						assert!(false, "{}", e);
					}
				}
			}
			Err(e) => {
				assert!(false, "{}", e);
			}
		}
    }

    // # [test]
    fn init_lic() {
		match License::load("tests/test_rdbm.license") {
			Ok(l) => {
				let c = l.cnt();
				assert_eq!(c, 6);
				match l.digest() { // test
					Ok(d) => {
						assert!(d.0.len() > 0)
					}
					Err(e) => {
						assert!(false, "{}", e);
					}
				}
			}
			Err(e) => {
				assert!(false, "{}", e);
			}
		}
    }
}
