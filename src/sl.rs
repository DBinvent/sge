use std::{env, fs};
use rsa::{RSAPrivateKey, PaddingScheme};
use rand::thread_rng;
use sha1::Sha1;

use chrono::{Utc, Duration};

/// ssh-keygen -t rsa -m PKCS8  -f corp.rsa
fn main() -> Result<(), String> {
    let args = env::args();
	let mut files = Vec::new();
	env::args().for_each(|a| files.push(a));
	let (rsaf, file_name) = if args.len() > 2 {
		(files[1].to_owned(), files[2].to_owned())
	} else {
        println!("sign the license by: ./sl corp.rsa evaluation.license\n using default... ");
		("corp.rsa".to_string(), "evaluation.license".to_string())
	};

	let mut lic = rdbm::license::License::load(&file_name)?;
	if lic.license_type.as_str() == "Evaluation" {
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

	let (digest, _) = lic.digest()?;
	let sgn = rsa.sign_blinded(&mut thread_rng(),
		 PaddingScheme::new_pss::<Sha1, _>(thread_rng()), &digest)
			.map_err(|e| format!("sign license [{}] by [{}]: {}", file_name, rsaf, e))?;

    lic.license = base64::encode(sgn);

	fs::write(&file_name, serde_yaml::to_string(&lic).unwrap())
		.map_err(|e| format!("write [{}] {}", file_name, e))
}

