use std::collections::HashMap;
use yaml_rust::{YamlLoader, Yaml};

/// TODO: --schema_transaction:
//     - (column-TBD): perform all changes on a table per transaction. \\n \
//     - (repeat-TBD): try to obtain a lock perform changes repetally - suitable on high load systems \\n \

pub const CONFIGS: [&'static str; 4] = [
"en", include_str!("i18n/en_clc.yaml"),
"ru", include_str!("i18n/ru_clc.yaml"),
];

pub fn cl_config() -> &'static str {
    let l = lang();
    let mut x = 1;
    for i in 0..CONFIGS.len() / 2 {
        if CONFIGS[i * 2] == l {
            x = i * 2 + 1;
            break;
        }
    }
    CONFIGS[x]
}

/// loading all files unparsed to bundle structure at compile, will be parsed on constructor at start
static BUNDLS: [&'static str; 6] = [
"system", include_str!("i18n/system.yaml"),
"en", include_str!("i18n/en.yaml"),
"ru", include_str!("i18n/ru.yaml"),
];

type TextBundle = HashMap<String, HashMap<String, String>>;

pub struct Bundle {
    pub text: TextBundle,
}

impl Bundle {
    pub fn new(input: &Yaml) -> Self {
        let mut text: TextBundle = HashMap::new();
        if let Some(input) = input.as_hash() {
            for (scope, y) in input {
                if let Some(y) = y.as_hash() {
                    for (key, value) in y {
                        let scope = scope.as_str().unwrap_or("").to_string();
                        let key = key.as_str().unwrap_or("").to_string();
                        let value = value.as_str().unwrap_or("").to_string();
                        match text.get_mut(&scope) {
                            None => {
                                let mut vals = HashMap::new();
                                vals.insert(key, value);
                                text.insert(scope, vals);
                            }
                            Some(s) => {
                                s.insert(key, value);
                            }
                        }
                    }
                }
            }
        }
        Bundle {
            text
        }
    }

    #[inline]
    fn msg(&self, scope: &String, code: &String) -> Option<&String> {
        self.text.get(scope).and_then(|m| m.get(code))
    }
}

pub struct Translate {
    system_bundle: Bundle,
    translation: Bundle,
    default_bundle: Option<Bundle>,
}

impl Translate {
    pub fn new() -> Self {
        let l = lang();
        let mut x = 1;
        for i in 0..BUNDLS.len()/2 {
            if BUNDLS[i*2] == l {
                x = i*2+1;
                break;
            }
        }
        Translate {
            system_bundle: Bundle::new(&YamlLoader::load_from_str(BUNDLS[1]).unwrap()[0]),
            translation: Bundle::new(&YamlLoader::load_from_str(BUNDLS[x]).unwrap()[0]),
            default_bundle: if x>0 {Some(Bundle::new(&YamlLoader::load_from_str(BUNDLS[3]).unwrap()[0])) } else {None}
        }
    }

    #[inline]
    pub fn sys_code(&self, scope: &String, code: &String) -> String {
        self.system_bundle.msg(scope, code).unwrap_or(&"not found".to_string()).to_owned()
    }

    #[inline]
    pub fn t7e(&self, scope: &String, code: &String) -> String {
        match self.translation.msg(&scope.to_string(), &code.to_string()) {
            None => if let Some(b) = &self.default_bundle {
                b.msg(&scope.to_string(), &code.to_string())
                    .unwrap_or(&"not found".to_string()).to_owned()
            } else { "not found".to_string() },
            Some(t) => t.to_owned()
        }
    }

    #[inline]
    pub fn t7ea(&self, scope: &str, code: &str, args: &[&str], msgs: &[&String]) -> String {
        let mut msg = self.t7e(&scope.to_string(), &code.to_string());
        let mut i = 0;
        for a in args {
            let new = if a.as_bytes()[0] == b'$' {
                self.t7e(&scope.to_string(), &a[1..].to_string())
            } else { msgs[i].to_owned() };
            msg = msg.replace(*a, new.as_str());
            i += 1;
        }
        msg
    }
}

pub fn lang() -> String {
    let l = std::env::var("LANG").unwrap_or("".to_string());
    if l.len() > 2 {
        l[..2].to_string()
    } else {
        l
    }
}

#[cfg(test)]
#[allow(unused)]
mod tests {
    use super::*;

    #[test]
    fn test_load() {
        let tr = Translate::new();
        assert!(tr.system_bundle.text.len()>0);
        assert!(tr.translation.text.len()>0);

        let msg = tr.t7ea("license", "err7", &["call"], &[&".".to_string()]);

        println!("{}\n {}", msg, tr.t7ea("license", "good", &[], &[]));
    }

    #[test]
    fn test_load_nol() {
        std::env::set_var("LANG", "");

        let tr = Translate::new();
        assert!(tr.system_bundle.text.len()>0);
        assert!(tr.translation.text.len()>0);

        let msg = tr.t7ea("license", "err7", &["call"], &[&".".to_string()]);
        assert!(cl_config().len() > 20);

    }

    #[test]
    fn test_load_en() {
        std::env::set_var("LANG", "en");

        let tr = Translate::new();
        assert!(tr.system_bundle.text.len()>0);
        assert!(tr.translation.text.len()>0);

        let msg = tr.t7ea("license", "err7", &["call"], &[&".".to_string()]);
        assert!(cl_config().len() > 20);

    }


}

