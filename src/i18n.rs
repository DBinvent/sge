use std::collections::HashMap;
use yaml_rust::{YamlLoader, Yaml};

/// loading all files unparsed to bundle structure at compile, will be parsed on constructor at start
static BUNDLS: [&'static str; 6] = [
"default", include_str!("i18n/default.yaml"),
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
        println!("{:?}", text);
        Bundle {
            text
        }
    }
}

pub struct Translate {
    default_bundle: Bundle,
    translation: Bundle,
    #[allow(unused)]
    not_translated: bool
}

impl Translate {
    pub fn new() -> Self {
        let l = lang();
        let mut x = 0;
        for i in 0..BUNDLS.len()/2 {
            if BUNDLS[i*2] == l {
                x = i*2+1;
                break;
            }
        }
        Translate {
            default_bundle: Bundle::new(&YamlLoader::load_from_str(BUNDLS[1]).unwrap()[0]),
            translation: Bundle::new(&YamlLoader::load_from_str(BUNDLS[x]).unwrap()[0]),
            not_translated: x == 0 && l.as_str() != "en",
        }
    }

    pub fn translate(&self, scope: &String, code: &String, do_it: bool) -> String {
        let bundle = if do_it {
            &self.translation.text
        } else {
            &self.default_bundle.text
        };
        match bundle.get(scope) {
            None =>  "not found".to_string(),
            Some(t) => match t.get(code) {
                None => "not found".to_string(),
                Some(v) => v.to_owned()
            }
        }
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
        assert!(tr.default_bundle.text.len()>0);
        assert!(tr.translation.text.len()>0);
    }

}

