use std::{env, fs};
use rsa::RSAPrivateKey;
use rand::thread_rng;

use chrono::{Utc, Duration};
use rsa::padding::PaddingScheme::PKCS1v15Sign;
use rdbm::license;
use rdbm::license::Connection;
use std::process::Command;

/// ssh-keygen -t rsa -m PKCS8  -f new.rsa
fn main() -> Result<(), String> {
    let args = env::args();
	let mut files = Vec::new();
	env::args().for_each(|a| files.push(a));
	let (rsaf,  file_name) = if args.len() > 2 {
		(files[1].to_owned(), files[2].to_owned())
	} else {
        println!("sign the license by: ./sl corp0.rsa evaluation.license\n using default... ");
		("keys/corp0.rsa".to_string(), "evaluation.license".to_string())
	};
	let mut eval = false;
	let mut lic = rdbm::license::License::load(&file_name)?;
	{
		if lic.connections.len() == 0 {
			lic.connections.push(Connection::new(Utc::today().naive_utc(), 1));
		} else {
			for c in &mut lic.connections {
				if c.state == "Evaluation" {
					c.valid_until = Utc::today().naive_utc() + Duration::days(32);
					eval = true;
				}
			}
		}
		lic.api_key_valid_until = Utc::now().naive_utc();
	}
	if eval {
		lic.key = openssl_genrsa()?;
	}
	println!("signing license: {} {} {}", lic.license_type, lic.issued_to, lic.email);
	println!("{:?}", lic.connections);

	let input = fs::read_to_string(rsaf.clone())
		.map_err(|e| format!("read file {}: {}", rsaf, e))?
		.lines()
		.filter(|line| !line.starts_with("-"))
		.fold(String::new(), |mut data, line| {
			data.push_str(&line);
			data
		});
	if input.len() < 2000 {
		return Err(format!("bad rsa [{}]: {}", rsaf, input));
	}

	let encoded = base64::decode(&input)
		.map_err(|e| format!("key decode error [{}] {} ", rsaf, e))?;
	let rsa = RSAPrivateKey::from_pkcs8(&encoded)
			.map_err(|e| format!("key create error [{}] {:?}", rsaf, e))?;

	let digest =  lic.pre_digest(&rsa.to_public_key()).digest()?;
	let sgn = rsa.sign_blinded(&mut thread_rng(),
                PKCS1v15Sign { hash: license::SIGN_SH }, &digest)
			.map_err(|e| format!("sign license [{}] by [{}]: {}", file_name, rsaf, e))?;
	rdbm::license::crc::debug("signed", &digest, &sgn, &rsa.to_public_key());

    lic.license = base64::encode(sgn);
	if eval {
		lic.key = "".to_string();
	}
	fs::write(&file_name, serde_yaml::to_string(&lic).unwrap())
		.map_err(|e| format!("write [{}] {}", file_name, e))
}

fn openssl_genrsa() -> Result<String, String> {
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
#[allow(dead_code)]
mod tests {
    use super::*;
	use rsa::{RSAPublicKey, PublicKeyParts};

	pub fn pub2str(input: &RSAPublicKey) -> String {
		format!(
			"{}{}",
			base64::encode(input.n().to_bytes_be()),
			base64::encode(input.e().to_bytes_be()),
		)
	}

	// используется для генерации pub ключа после ssh-keygen
    // #[test]
    fn pubk() -> Result<(), String> {
		let rsaf = "tests/new.rsa";
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
		let rsa = RSAPrivateKey::from_pkcs8(&encoded)
				.map_err(|e| format!("key create error [{}] {:?}", rsaf, e))?;

		println!("{} PUB:[{}]", rsaf, pub2str(&rsa.to_public_key()));

        Ok(())
    }
}
