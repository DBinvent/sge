/// TODO: --schema_transaction:
//     - (column-TBD): perform all changes on a column per transaction.
//     - (retry-TBD): try to obtain a lock perform changes repeatably - suitable on high load systems src/schema_guard:1308

/// [i18n] by LANG's first two letters; fr, de, zh (Simplified) and ja are not reviewed
/// translations and say so in their `about`
pub const CONFIGS: [&'static str; 12] = [
"en", include_str!("i18n/en_clc.yaml"),
"ru", include_str!("i18n/ru_clc.yaml"),
"fr", include_str!("i18n/fr_clc.yaml"),
"de", include_str!("i18n/de_clc.yaml"),
"zh", include_str!("i18n/zh_clc.yaml"),
"ja", include_str!("i18n/ja_clc.yaml"),
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

lazy_static::lazy_static! {
    /// set_lang's
    static ref LANG: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);
}

/// [i18n] The language of the messages from here on - and of the command line, when it
/// is set before - `en`, `ru`, `fr`, `de`, `zh`, `ja`, or a locale of one. Wins over LANG.
pub fn set_lang(lang: &str) {
    if let Ok(mut l) = LANG.write() {
        *l = Some(lang.chars().take(2).collect::<String>().to_lowercase());
    }
    // schema_guard's messages in it too: its set_lang wins over the logger's lang
    schema_guard_tokio::set_lang(lang);
}

/// The language: set_lang's, else LANG's first two letters. One without its texts -
/// none, `C.UTF-8` - is English (cl_config_for, rust-i18n's fallback)
pub fn lang() -> String {
    if let Some(l) = LANG.read().ok().and_then(|l| l.clone()) {
        return l;
    }
    let l = std::env::var("LANG").unwrap_or("".to_string());
    if l.len() > 2 {
        l[..2].to_string()
    } else {
        l
    }
}

/// [i18n] The message `key` (locales/*.yml) in the language of lang(), its %{name}
/// placeholders filled from `args`: tr! for a crate that has not the messages - the
/// command line's
pub fn message(key: &str, args: &[(&str, &str)]) -> String {
    let mut m = rust_i18n::t!(key, locale = &lang()).into_owned();
    for (name, value) in args {
        m = m.replace(&format!("%{{{}}}", name), value);
    }
    m
}

/// [i18n] The message `key` in the language of [lang], each %{name} placeholder filled
macro_rules! tr {
    ($key:expr $(, $name:ident = $value:expr)* $(,)?) => {
        rust_i18n::t!($key, locale = &$crate::i18n::lang() $(, $name = $value)*).into_owned()
    };
}

#[cfg(test)]
#[allow(unused)]
mod tests {
    use super::*;
    use yaml_rust2::{Yaml, YamlLoader};

    // [lang] by cl_config_for, never LANG: the tests run in parallel, and one setting
    // LANG changed what another read

    /// locales/*.yml, as key => language => message
    fn messages() -> std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>> {
        let dir = format!("{}/locales", env!("CARGO_MANIFEST_DIR"));
        let mut all = std::collections::BTreeMap::new();
        for f in std::fs::read_dir(&dir).unwrap() {
            let f = f.unwrap().path();
            let y = &YamlLoader::load_from_str(&std::fs::read_to_string(&f).unwrap()).unwrap()[0];
            for (k, v) in y.as_hash().unwrap() {
                let k = k.as_str().unwrap();
                if k == "_version" {
                    continue;
                }
                let langs = v.as_hash().unwrap_or_else(|| panic!("{}: {}", f.display(), k)).iter()
                    .map(|(l, m)| (l.as_str().unwrap().to_string(), m.as_str().unwrap().to_string())).collect();
                assert!(all.insert(k.to_string(), langs).is_none(), "{} twice", k);
            }
        }
        all
    }

    /// [i18n] every message in every language the command line speaks, with the same
    /// placeholders; an error's code the same in every language, and no two errors
    /// with one code
    #[test]
    fn every_message_is_in_every_language() {
        let placeholders = |m: &str| {
            let mut p: Vec<String> = m.split("%{").skip(1).map(|r| r.split('}').next().unwrap().to_string()).collect();
            p.sort();
            p
        };
        let mut langs_cl: Vec<&str> = CONFIGS.iter().step_by(2).copied().collect();
        langs_cl.sort();
        let mut codes = std::collections::BTreeMap::new();
        for (key, langs) in messages() {
            let en = &langs["en"];
            assert_eq!(langs.keys().map(String::as_str).collect::<Vec<_>>(), langs_cl, "{}", key);
            let code = en.split(' ').next().filter(|c| c.starts_with("SGE-E"));
            for (l, m) in &langs {
                assert_eq!(placeholders(m), placeholders(en), "{} {}", key, l);
                assert_eq!(m.split(' ').next().filter(|c| c.starts_with("SGE-E")), code, "{} {}: the code", key, l);
            }
            if let Some(c) = code {
                assert!(codes.insert(c.to_string(), key.clone()).is_none(), "{} for {} and {}", c, codes[c], key);
            }
        }
        assert!(codes.len() > 80, "{}", codes.len());
    }

    /// [i18n] every key the code asks for is in locales/, and every message there asked for
    #[test]
    fn every_key_used_has_its_message_and_every_message_a_use() {
        let all = messages();
        let is_key = |s: &str| ["lib.", "main.", "cfg.", "files.", "log.", "etl.", "git.", "zip.", "pg.", "digest."].iter()
            .any(|p| s.starts_with(p) && s.len() > p.len());
        let mut used = std::collections::BTreeSet::new();
        for f in std::fs::read_dir(format!("{}/src", env!("CARGO_MANIFEST_DIR"))).unwrap() {
            let f = f.unwrap().path();
            if f.extension() != Some(std::ffi::OsStr::new("rs")) {
                continue;
            }
            let src = std::fs::read_to_string(&f).unwrap();
            // every "key" literal: from each quote, the key characters up to the closing one
            for (i, _) in src.match_indices('"') {
                let rest = &src[i + 1..];
                let end = rest.find(|c: char| !(c.is_ascii_lowercase() || c == '_' || c == '.')).unwrap_or(0);
                let literal = &rest[..end];
                if rest[end..].starts_with('"') && is_key(literal) {
                    assert!(all.contains_key(literal), "{}: no message {}", f.display(), literal);
                    used.insert(literal.to_string());
                }
            }
        }
        let unused: Vec<&String> = all.keys().filter(|k| !used.contains(*k)).collect();
        assert!(unused.is_empty(), "messages nothing asks for: {:?}", unused);
    }

    #[test]
    fn a_language_there_is_no_file_for_is_english() {
        assert_eq!(cl_config_for(""), cl_config_for("en"));
        assert_eq!(cl_config_for("xx"), cl_config_for("en"));
        assert_ne!(cl_config_for("ru"), cl_config_for("en"));
    }

    /// [man] every language's part of the man page, man/<lang>.h2m: the NAME line, as many
    /// examples as the English one, and a row in man/build.sh, which writes the pages
    #[test]
    fn every_language_has_its_man_page() {
        let read = |f: &str| std::fs::read_to_string(format!("{}/man/{}", env!("CARGO_MANIFEST_DIR"), f))
            .unwrap_or_else(|e| panic!("man/{}: {}", f, e));
        let examples = |t: &str| t.lines().filter(|l| l.starts_with(".SS \"")).count();
        let (en, build) = (read("en.h2m"), read("build.sh"));
        assert!(examples(&en) > 10);
        for l in CONFIGS.iter().step_by(2) {
            let t = read(&format!("{}.h2m", l));
            assert!(t.starts_with("[NAME]\nsge \\- ") && t.contains("\n[examples]\n"), "man/{}.h2m", l);
            assert_eq!(examples(&t), examples(&en), "man/{}.h2m", l);
            assert!(build.contains(&format!("\n{}|", l)), "man/build.sh makes no {} page", l);
        }
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
        let en = names("en");
        for l in CONFIGS.iter().step_by(2) {
            assert_eq!(en, names(l), "{}", l);
        }
        assert_eq!(en.0, vec!["info", "migrate", "plan", "prepare", "snapshot"]);
        assert!(!en.1.iter().any(|a| a.contains("licen") || a == "email" || a == "subscribe"), "{:?}", en.1);
    }
}
