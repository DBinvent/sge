use std::collections::HashMap;
use yaml_rust2::{YamlLoader, Yaml};

/// TODO: --schema_transaction:
//     - (column-TBD): perform all changes on a column per transaction.
//     - (retry-TBD): try to obtain a lock perform changes repeatably - suitable on high load systems src/schema_guard:1308

pub const CONFIGS: [&'static str; 4] = [
"en", include_str!("i18n/en_clc.yaml"),
"ru", include_str!("i18n/ru_clc.yaml"),
];

pub fn cl_config() -> &'static str {
    cl_config_for(&lang())
}

/// The command line of language `l`, English when there is none
pub fn cl_config_for(l: &str) -> &'static str {
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
static BUNDLS: [&'static str; 4] = [
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
    translation: Bundle,
    default_bundle: Option<Bundle>,
}

impl Translate {
    pub fn new() -> Self {
        Self::for_lang(&lang())
    }

    /// The texts of language `l`, English for what it lacks and when there is none
    pub fn for_lang(l: &str) -> Self {
        let mut x = 1;
        for i in 0..BUNDLS.len()/2 {
            if BUNDLS[i*2] == l {
                x = i*2+1;
                break;
            }
        }
        Translate {
            translation: Bundle::new(&YamlLoader::load_from_str(BUNDLS[x]).unwrap()[0]),
            default_bundle: Some(Bundle::new(&YamlLoader::load_from_str(BUNDLS[1]).unwrap()[0])),
        }
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
            let new = if a.as_bytes()[1] == b'$' {
                self.t7e(&scope.to_string(), &a[2..a.len()-1].to_string())
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

    // [lang] by Translate::for_lang and cl_config_for, never LANG: the tests run in
    // parallel, and one setting LANG changed what another read

    /// a message with its placeholders filled - a value and a {$key} reference
    fn unknown(tr: &Translate, cmd: &str) -> String {
        tr.t7ea("cl", "unknown", &["{cmd}", "{$usage_hint}"], &[&cmd.to_string()])
    }

    #[test]
    fn test_load() {
        let tr = Translate::for_lang("en");
        assert!(tr.translation.text.len()>0);
        let msg = unknown(&tr, "x");
        assert!(msg.contains("x") && !msg.contains('{'), "{}", msg);
    }

    #[test]
    fn test_load_nol() {
        // no language: English
        let tr = Translate::for_lang("");
        assert!(tr.translation.text.len()>0);
        assert!(unknown(&tr, "x").starts_with("Unknown command: x"));
        assert!(cl_config_for("").len() > 20);
    }

    #[test]
    fn test_load_en() {
        let tr = Translate::for_lang("en");
        assert!(tr.translation.text.len()>0);
        assert!(unknown(&tr, "x").starts_with("Unknown command: x"));
        assert!(cl_config_for("en").len() > 20);
    }

    #[test]
    fn test_load_en1() {
        // {$usage_hint} is the text of cl.usage_hint, in the same language
        let tr = Translate::for_lang("en");
        let msg = unknown(&tr, "x");
        assert!(matches!(msg.find("{$usage_hint}"), None), "{}", msg);
        assert!(msg.ends_with(tr.t7e(&"cl".to_string(), &"usage_hint".to_string()).as_str()), "{}", msg);
    }

    #[test]
    fn test_load_en7() {
        let tr = Translate::for_lang("en");
        let msg = unknown(&tr, "licence");
        assert!(matches!(msg.find("{cmd}"), None), "{}", msg);
        assert!(msg.contains("licence"), "{}", msg);
    }

    #[test]
    fn russian_and_an_unknown_language() {
        let ru = Translate::for_lang("ru");
        let msg = unknown(&ru, "x");
        assert!(msg.starts_with("Неизвестная команда: x"), "{}", msg);
        assert!(!msg.contains('{'), "{}", msg);
        // a language there is no file for: English
        assert!(unknown(&Translate::for_lang("xx"), "x").starts_with("Unknown command: x"));
    }

    #[test]
    fn every_language_has_the_same_commands_and_options() {
        let names = |l: &str| {
            let y = &YamlLoader::load_from_str(cl_config_for(l)).unwrap()[0];
            let keys = |list: &Yaml| -> Vec<String> {
                let mut v: Vec<String> = list.as_vec().map(|items| items.iter()
                    .filter_map(|i| i.as_hash().and_then(|h| h.keys().next()).and_then(|k| k.as_str()).map(String::from))
                    .collect()).unwrap_or_default();
                v.sort();
                v
            };
            (keys(&y["subcommands"]), keys(&y["args"]))
        };
        let (en, ru) = (names("en"), names("ru"));
        assert_eq!(en, ru);
        assert_eq!(en.0, vec!["info", "migrate", "prepare", "snapshot"]);
        assert!(!en.1.iter().any(|a| a.contains("licen") || a == "email" || a == "subscribe"), "{:?}", en.1);
    }
}

