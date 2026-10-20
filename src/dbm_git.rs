//! [git] A git repository as a script source.
//!
//! sge reads migration scripts from `source_dir`. When `source_git` is set the
//! repository is materialised into a local cache first and `source_dir` is
//! resolved *inside* it, so everything downstream - filters, version parsing,
//! checksums, history - is untouched and behaves exactly as it does for a
//! directory on disk. Leaving `source_git` empty keeps the previous behaviour
//! bit for bit.
//!
//! ## Why shell out to `git`
//!
//! For the same reason sge shells out to `psql` rather than embedding a
//! Postgres client: it keeps the binary small (~10 MB is a stated selling
//! point), and - more importantly - it inherits the operator's existing
//! credentials with no second auth model to configure. ssh-agent, credential
//! helpers, `~/.gitconfig`, `insteadOf` rewrites, proxies and CA bundles all
//! work because it is the same `git` the operator already uses. A linked
//! implementation would mean re-solving authentication badly.
//!
//! ## Configuration
//!
//! | key | meaning |
//! |---|---|
//! | `source_git` | repository URL. Empty disables all of this. |
//! | `source_git_ref` | branch, tag or commit. Default: the remote's HEAD. |
//! | `source_git_dir` | sub-directory inside the repo holding the scripts. |
//! | `source_git_cache` | where clones live. Default `<tmp>/sge-git`. |
//! | `source_git_depth` | shallow depth, default 1. `0` fetches full history. |
//! | `source_git_ssh_key` | identity file for `git@host:repo` URLs. Empty leaves ssh-agent and `~/.ssh/config` in charge. |
//! | `source_git_known_hosts` | known-hosts file. Default: `known_hosts` beside the cache. |
//!
//! ## git over ssh
//!
//! An ssh URL (`git@host:org/repo.git`, `ssh://git@host/repo.git`) needs
//! nothing beyond `source_git` when the runner already has a working agent.
//! Set `source_git_ssh_key` to pin one identity - a deploy key - and ssh is
//! run with `BatchMode=yes`, so a key that is missing, wrong or
//! passphrase-protected **fails** instead of waiting for a passphrase nobody
//! is there to type. See [`ssh_command`] for why each option is there.
//!
//! ## What is recorded
//!
//! The resolved commit is returned so the caller can put it in the history log.
//! That is the point of sourcing from git at all: `sge_history.script_tag` then
//! answers "which revision produced this database", which a directory path
//! cannot.

use std::path::{Path, PathBuf};
use std::process::Command;

use slog::Logger;

use crate::dbm_cfg::ConfigMap;

/// A materialised checkout: where it is, and exactly what it is.
#[derive(Debug, Clone)]
pub(crate) struct GitSource {
    /// directory the scripts are read from - repo root joined with `source_git_dir`
    pub(crate) path: String,
    /// full resolved commit sha of the checkout
    pub(crate) commit: String,
    /// what was asked for: branch, tag, sha, or "HEAD"
    pub(crate) requested: String,
    pub(crate) url: String,
}

impl GitSource {
    /// Short sha, for logs and for `script_tag`.
    pub(crate) fn short(&self) -> &str {
        if self.commit.len() >= 12 { &self.commit[..12] } else { self.commit.as_str() }
    }
}

/// A URL that begins with `-` would be read by git as a flag no matter how the
/// arguments are passed. Every other injection route is closed by using
/// `Command` with separate arguments and never a shell string.
fn sane_url(url: &str) -> Result<(), String> {
    if url.starts_with('-') {
        return Err(format!("source_git must not start with '-': {}", redact(url)));
    }
    if url.contains('\0') {
        return Err("source_git contains a NUL byte".to_string());
    }
    Ok(())
}

/// [redact] The URL as a log may show it: the password of `scheme://user:pass@host/`
/// replaced, a user part with no password - a token as the user - replaced whole.
/// `git@host:repo` carries no secret and stays.
fn redact(url: &str) -> String {
    match userinfo(url) {
        None => url.to_string(),
        Some((scheme, info, rest)) => {
            let shown = match info.find(':') {
                Some(i) => format!("{}:***", &info[..i]),
                None => "***".to_string(),
            };
            format!("{}://{}@{}", scheme, shown, rest)
        }
    }
}

/// [redact] The URL with no user part at all, for the cache directory's name
fn without_userinfo(url: &str) -> String {
    match userinfo(url) {
        None => url.to_string(),
        Some((scheme, _, rest)) => format!("{}://{}", scheme, rest),
    }
}

/// `scheme://info@rest` split into its scheme, its user part and the rest
fn userinfo(url: &str) -> Option<(&str, &str, &str)> {
    let i = url.find("://")?;
    let after = &url[i + 3..];
    let host_end = after.find('/').unwrap_or(after.len());
    let at = after[..host_end].rfind('@')?;
    Some((&url[..i], &after[..at], &after[at + 1..]))
}

/// Stable, filesystem-safe directory name for a repository URL.
///
/// Not a hash: a human looking in the cache should be able to tell which
/// repository a directory belongs to without running anything.
fn cache_name(url: &str) -> String {
    let mut o = String::with_capacity(url.len());
    for c in url.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            o.push(c);
        } else if !o.ends_with('_') {
            o.push('_');
        }
    }
    o.trim_matches('_').to_string()
}

lazy_static::lazy_static! {
    /// The revision this run is deploying from, once resolved.
    ///
    /// A process-wide slot rather than a threaded-through parameter because the
    /// value is a property of the *run*, and the history writer that needs it
    /// sits several layers below the loader that discovers it. sge runs one
    /// migration per process, so there is exactly one answer.
    static ref REVISION: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);
}

/// Record the revision so history rows can be tagged with it.
pub(crate) fn remember(g: &GitSource) {
    if let Ok(mut w) = REVISION.write() {
        *w = Some(g.short().to_string());
    }
}

/// The short commit this run deployed from, or `None` for a filesystem source.
///
/// This is the whole point of a git source: `sge_history` can then answer
/// "which revision produced this database", which a directory path cannot.
pub(crate) fn revision() -> Option<String> {
    REVISION.read().ok().and_then(|r| r.clone())
}

/// A key path reaches ssh through `GIT_SSH_COMMAND`, which git splits with
/// shell-like quoting. Quoting the path covers spaces; a path that could close
/// the quote is refused rather than escaped, because escaping rules differ
/// between git's parser and a shell and the difference is a command injection.
fn sane_key(path: &str) -> Result<(), String> {
    if path.contains('"') || path.contains('\'') || path.contains('\\')
        || path.contains('\n') || path.contains('\0') {
        return Err(format!("source_git_ssh_key must not contain quotes or newlines: {}", path));
    }
    Ok(())
}

/// The ssh command git should use for this run.
///
/// `GIT_TERMINAL_PROMPT=0` above silences *git's* credential prompt and says
/// nothing about ssh. Without `BatchMode=yes` a missing or passphrase-protected
/// key makes ssh ask - and a migration run in CI has no terminal to ask, so the
/// clone hangs until something times out. That is the same failure shape the
/// terminal-prompt setting exists to prevent, one layer down.
///
/// `StrictHostKeyChecking=accept-new` rather than `no`: trust the host on first
/// contact, but still refuse if its key later *changes*. The known-hosts file
/// lives beside the clone cache because a CI runner's `HOME` is often
/// ephemeral, and ssh silently records nothing when it cannot write the default
/// path - which turns first-use trust into every-use trust.
///
/// `IdentitiesOnly=yes` accompanies an explicit key so ssh offers that key and
/// not every identity in the agent: a server counting failed attempts
/// disconnects before reaching the one that would have worked. Without a
/// configured key the agent is left to do its job, which is what makes
/// ssh-agent and `~/.ssh/config` keep working untouched.
fn ssh_command(key: &str, known_hosts: &str) -> Result<String, String> {
    let key = key.trim();
    let kh = known_hosts.trim();
    sane_key(kh)?;
    let mut c = format!(
        "ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=\"{}\"",
        kh);
    if !key.is_empty() {
        sane_key(key)?;
        c.push_str(&format!(" -o IdentitiesOnly=yes -i \"{}\"", key));
    }
    Ok(c)
}

fn git(args: &[&str], cwd: Option<&Path>, log: &Logger) -> Result<String, String> {
    git_env(args, cwd, None, log)
}

fn git_env(args: &[&str], cwd: Option<&Path>, ssh: Option<&str>, log: &Logger)
    -> Result<String, String>
{
    git_run(args, cwd, ssh, None, log)
}

/// `git`, with an optional ssh command and an optional credential file.
///
/// The credential file is passed as a **path**, in `-c credential.helper=store
/// --file=<path>`. The path in argv is harmless - it is not the secret - while
/// the token itself never leaves the 0600 file git reads. Putting the token in
/// argv instead (`-c http.extraHeader=...`) would publish it to every local
/// user through `/proc/<pid>/cmdline` for as long as the clone runs.
fn git_run(args: &[&str], cwd: Option<&Path>, ssh: Option<&str>, cred: Option<&str>,
    log: &Logger) -> Result<String, String>
{
    let mut c = Command::new("git");
    // Never prompt. A migration run is usually non-interactive, and a hung
    // credential prompt looks exactly like a slow network.
    c.env("GIT_TERMINAL_PROMPT", "0");
    if let Some(s) = ssh {
        c.env("GIT_SSH_COMMAND", s);
    }
    // `-c` has to precede the subcommand, so the helper is prepended rather
    // than appended.
    let helper;
    let mut argv: Vec<&str> = Vec::with_capacity(args.len() + 2);
    if let Some(path) = cred {
        sane_key(path)?;
        // git splits the helper string on whitespace before running it, so a
        // path containing any would become two arguments and the credential
        // would silently not be found.
        if path.split_whitespace().count() != 1 {
            return Err(format!(
                "source_git_credentials must not contain whitespace: {}", path));
        }
        helper = format!("credential.helper=store --file={}", path);
        argv.push("-c");
        argv.push(helper.as_str());
    }
    argv.extend_from_slice(args);
    let args = &argv[..];
    c.args(args);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    trace!(log, "git {}", args.join(" "));
    let out = c
        .output()
        .map_err(|e| format!("git {}: {} (is git installed and on PATH?)", args.join(" "), e))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Clone if absent, fetch if present, then pin to the requested ref.
///
/// Returns `Ok(None)` when no git source is configured, which is the ordinary
/// filesystem case and must stay free of surprises.
pub(crate) fn materialize(cfg: &ConfigMap, log: &Logger) -> Result<Option<GitSource>, String> {
    let url = cfg.get("source_git");
    if url.trim().is_empty() {
        return Ok(None);
    }
    let url = url.trim().to_string();
    sane_url(&url)?;

    let requested = {
        let r = cfg.get("source_git_ref");
        if r.trim().is_empty() { "HEAD".to_string() } else { r.trim().to_string() }
    };
    let depth = cfg.get("source_git_depth");
    let depth: u32 = if depth.trim().is_empty() { 1 } else { depth.trim().parse().unwrap_or(1) };

    let cache_root = {
        let c = cfg.get("source_git_cache");
        if c.trim().is_empty() {
            std::env::temp_dir().join("sge-git")
        } else {
            PathBuf::from(c.trim())
        }
    };
    std::fs::create_dir_all(&cache_root)
        .map_err(|e| format!("source_git_cache {}: {}", cache_root.display(), e))?;
    let repo = cache_root.join(cache_name(&without_userinfo(&url)));

    // Configured for every URL, not only ones that look like ssh: the scheme a
    // repository is reached by can change (an `insteadOf` rewrite, a mirror)
    // without the configuration changing, and an unused GIT_SSH_COMMAND costs
    // an https clone nothing.
    let kh = match cfg.get("source_git_known_hosts") {
        s if s.trim().is_empty() => cache_root.join("known_hosts").display().to_string(),
        s => s.trim().to_string(),
    };
    let ssh = ssh_command(&cfg.get("source_git_ssh_key"), &kh)?;
    let ssh = Some(ssh.as_str());
    // A credential FILE for an http(s) remote, staged by the caller at 0600.
    // Empty for every ssh remote and for an unauthenticated one, which is the
    // ordinary case; an unused helper costs a clone nothing.
    let cred = cfg.get("source_git_credentials");
    let cred = match cred.trim() {
        "" => None,
        p => Some(p),
    };

    if repo.join(".git").is_dir() {
        info!(log, "[git] updating {} in {}", redact(&url), repo.display());
        // The remote may have been repointed in config since the last run;
        // silently reusing a clone of a different repository would deploy the
        // wrong scripts, which is the worst outcome available here.
        let have = git(&["remote", "get-url", "origin"], Some(&repo), log).unwrap_or_default();
        if have != url {
            warn!(log, "[git] origin changed {} -> {}, re-cloning", redact(&have), redact(&url));
            std::fs::remove_dir_all(&repo)
                .map_err(|e| format!("replacing {}: {}", repo.display(), e))?;
            clone(&url, &repo, depth, ssh, cred, log)?;
        } else {
            fetch(&repo, &requested, depth, ssh, cred, log)?;
        }
    } else {
        info!(log, "[git] cloning {} into {}", redact(&url), repo.display());
        clone(&url, &repo, depth, ssh, cred, log)?;
    }

    checkout(&repo, &requested, log)?;
    let commit = git(&["rev-parse", "HEAD"], Some(&repo), log)?;

    // `source_git_dir` is joined, not substituted: the scripts live inside the
    // checkout, and a path escaping it is a configuration error rather than a
    // feature.
    let sub = cfg.get("source_git_dir");
    let sub = sub.trim().trim_start_matches('/');
    if sub.split('/').any(|p| p == "..") {
        return Err(format!("source_git_dir must stay inside the repository: {}", sub));
    }
    let path = if sub.is_empty() { repo.clone() } else { repo.join(sub) };
    if !path.is_dir() {
        return Err(format!(
            "source_git_dir '{}' does not exist in {} at {}",
            sub, url, &commit[..commit.len().min(12)]
        ));
    }

    let src = GitSource {
        path: format!("{}/", path.display()),
        commit,
        requested,
        url,
    };
    info!(log, "[git] source {} @ {} ({})", redact(&src.url), src.short(), src.requested);
    Ok(Some(src))
}

fn clone(url: &str, repo: &Path, depth: u32, ssh: Option<&str>, cred: Option<&str>,
    log: &Logger) -> Result<(), String>
{
    let dst = repo.display().to_string();
    let d = depth.to_string();
    // Cloned without --branch so that a branch, a tag and a bare commit sha all
    // take the same path through `checkout` below; `--branch` rejects a sha.
    let mut args = vec!["clone", "--no-tags", "--quiet"];
    if depth > 0 {
        args.push("--depth");
        args.push(d.as_str());
    }
    args.push("--");
    args.push(url);
    args.push(dst.as_str());
    git_run(&args, None, ssh, cred, log).map(|_| ())
}

fn fetch(repo: &Path, requested: &str, depth: u32, ssh: Option<&str>, cred: Option<&str>,
    log: &Logger) -> Result<(), String>
{
    let d = depth.to_string();
    let mut args = vec!["fetch", "--quiet", "--force"];
    if depth > 0 {
        args.push("--depth");
        args.push(d.as_str());
    }
    args.push("origin");
    if requested != "HEAD" {
        args.push(requested);
    }
    // A shallow clone cannot always fetch an arbitrary commit - servers may
    // refuse a sha that is not a ref tip. Deepening is the documented recovery
    // and costs bandwidth, not correctness, so it is attempted once rather than
    // failing the run.
    if git_run(&args, Some(repo), ssh, cred, log).is_err() {
        warn!(log, "[git] shallow fetch of '{}' refused, unshallowing", requested);
        let _ = git_run(&["fetch", "--quiet", "--unshallow"], Some(repo), ssh, cred, log);
        git_run(&["fetch", "--quiet", "--force", "origin"], Some(repo), ssh, cred, log)?;
    }
    Ok(())
}

fn checkout(repo: &Path, requested: &str, log: &Logger) -> Result<(), String> {
    // FETCH_HEAD first: it is what the fetch above just resolved, and it works
    // uniformly for a branch, a tag and a sha. Falling back to the literal ref
    // covers the fresh-clone case where nothing has been fetched yet.
    let target = if requested == "HEAD" {
        git(&["rev-parse", "HEAD"], Some(repo), log)?
    } else {
        git(&["rev-parse", "FETCH_HEAD"], Some(repo), log)
            .or_else(|_| git(&["rev-parse", requested], Some(repo), log))
            .map_err(|e| format!("source_git_ref '{}' not found: {}", requested, e))?
    };
    // --hard, because the cache is sge's to own: anything modified in it was
    // not put there deliberately, and silently deploying a dirty checkout is
    // how a machine ends up with scripts nobody can point at a revision.
    git(&["reset", "--hard", "--quiet", target.as_str()], Some(repo), log)?;
    git(&["clean", "-qfdx"], Some(repo), log)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_that_looks_like_a_flag_is_refused() {
        // `--upload-pack=...` as a URL is remote code execution, and argument
        // separation alone does not stop git reading a leading dash as a flag
        assert!(sane_url("--upload-pack=touch /tmp/pwned").is_err());
        assert!(sane_url("https://example.com/r.git").is_ok());
        assert!(sane_url("git@example.com:o/r.git").is_ok());
    }

    #[test]
    fn credentials_in_a_url_reach_neither_logs_nor_the_cache_dir() {
        assert_eq!(redact("https://bob:s3cret@example.com/o/r.git"), "https://bob:***@example.com/o/r.git");
        assert_eq!(redact("https://ghp_token@example.com/o/r.git"), "https://***@example.com/o/r.git");
        assert_eq!(redact("https://example.com/o/r.git"), "https://example.com/o/r.git");
        assert_eq!(redact("git@example.com:o/r.git"), "git@example.com:o/r.git");
        // an @ in the path is not a user part
        assert_eq!(redact("https://example.com/o/r@v1.git"), "https://example.com/o/r@v1.git");
        assert_eq!(without_userinfo("https://bob:s3cret@example.com/o/r.git"), "https://example.com/o/r.git");
        assert!(!cache_name(&without_userinfo("https://bob:s3cret@example.com/o/r.git")).contains("s3cret"));
    }

    #[test]
    fn cache_names_are_readable_and_collision_free() {
        // a human should be able to see which repo a cache dir belongs to
        assert_eq!(cache_name("https://github.com/o/r.git"), "https_github.com_o_r.git");
        assert_ne!(
            cache_name("https://github.com/o/r.git"),
            cache_name("https://gitlab.com/o/r.git")
        );
    }

    #[test]
    fn ssh_never_waits_for_a_passphrase_nobody_can_type() {
        // the hang this prevents: ssh prompting in CI, which looks exactly
        // like a slow network until someone kills the job
        let c = ssh_command("", "/tmp/sge-git/known_hosts").unwrap();
        assert!(c.contains("BatchMode=yes"), "{}", c);
        assert!(c.contains("UserKnownHostsFile=\"/tmp/sge-git/known_hosts\""), "{}", c);
        // accept-new, not no: trust first use, still refuse a CHANGED host key
        assert!(c.contains("StrictHostKeyChecking=accept-new"), "{}", c);
        // no key configured: leave ssh-agent and ~/.ssh/config in charge
        assert!(!c.contains(" -i "), "{}", c);
        assert!(!c.contains("IdentitiesOnly"), "{}", c);
    }

    #[test]
    fn a_deploy_key_is_offered_alone() {
        let c = ssh_command("/etc/sge/deploy_ed25519", "/t/kh").unwrap();
        assert!(c.contains("-i \"/etc/sge/deploy_ed25519\""), "{}", c);
        // else a server counting failed attempts disconnects before reaching it
        assert!(c.contains("IdentitiesOnly=yes"), "{}", c);
    }

    #[test]
    fn a_key_path_that_could_break_out_of_the_quoting_is_refused() {
        // GIT_SSH_COMMAND is split with shell-like quoting, so a closing quote
        // in the path is command injection
        assert!(sane_key("/k\"; touch /tmp/pwned; \"").is_err());
        assert!(ssh_command("/k'x", "/t/kh").is_err());
        assert!(ssh_command("/etc/sge/deploy_ed25519", "/t/kh").is_ok());
    }

    #[test]
    fn short_sha_is_bounded_and_safe_on_junk() {
        let g = |c: &str| GitSource {
            path: "/tmp/".into(), commit: c.into(), requested: "HEAD".into(), url: "u".into(),
        };
        assert_eq!(g("0123456789abcdef0123").short(), "0123456789ab");
        // never panics on a short or empty commit, which a failed rev-parse gives
        assert_eq!(g("abc").short(), "abc");
        assert_eq!(g("").short(), "");
    }
}
