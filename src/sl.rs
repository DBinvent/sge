use std::{env, fs};
use rsa::{RSAPrivateKey, PaddingScheme, PublicKeyParts};
use rand::thread_rng;
use sha1::Sha1;

use chrono::{Utc, Duration};
use rdbm::license::License;

/// ssh-keygen -t rsa -m PKCS8  -f new.rsa
fn main() -> Result<(), String> {
    let args = env::args();
	let mut files = Vec::new();
	env::args().for_each(|a| files.push(a));
	let (rsaf,  file_name) = if args.len() > 2 {
		(files[1].to_owned(), files[2].to_owned())
	} else {
        println!("sign the license by: ./sl corp0.rsa evaluation.license\n using default... ");
		("corp0.rsa".to_string(), "evaluation.license".to_string())
	};

	let mut lic = rdbm::license::License::load(&file_name)?;
	if lic.license_type.starts_with("Evaluation_v") {
		lic.connects[0].valid_until = Utc::today().naive_utc() + Duration::days(32);
		lic.api_key_valid_until = Utc::now().naive_utc();
	}

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

	let digest = lic.digest()?;
	let sgn = rsa.sign_blinded(&mut thread_rng(),
		 PaddingScheme::new_pss::<Sha1, _>(thread_rng()), &digest)
			.map_err(|e| format!("sign license [{}] by [{}]: {}", file_name, rsaf, e))?;

    lic.license = base64::encode(sgn);
	lic.version = License::as_ver(&rsa.to_public_key());
	lic.license_type = lic.as_type();

	fs::write(&file_name, serde_yaml::to_string(&lic).unwrap())
		.map_err(|e| format!("write [{}] {}", file_name, e))
}


#[cfg(test)]
mod tests {
    use super::*;
	use rsa::RSAPublicKey;

	pub fn pub2str(input: &RSAPublicKey) -> String {
		format!(
			"{}{}",
			base64::encode(input.n().to_bytes_be()),
			base64::encode(input.e().to_bytes_be()),
		)
	}

    #[test]
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
