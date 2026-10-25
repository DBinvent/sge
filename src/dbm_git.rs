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
        return Err(tr!("git.flag_url", url = redact(url)));
    }
    if url.contains('\0') {
        return Err(tr!("git.nul"));
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
    remember_tag(g.short());
}

/// [zip] Any fetched source's own name for what it deployed - a commit here, an
/// archive's checksum for a zip (dbm_zip.rs) - tags the history rows the same way.
pub(crate) fn remember_tag(tag: &str) {
    if let Ok(mut w) = REVISION.write() {
        *w = Some(tag.to_string());
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
        return Err(tr!("git.key_quotes", path = path));
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
            return Err(tr!("git.cred_space", path = path));
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
        .map_err(|e| tr!("git.not_run", args = args.join(" "), err = e))?;
    if !out.status.success() {
        return Err(tr!("git.failed", args = args.join(" "), err = String::from_utf8_lossy(&out.stderr).trim()));
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
        .map_err(|e| tr!("git.cache", dir = cache_root.display(), err = e))?;
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
        info!(log, "{}", tr!("git.updating", url = redact(&url), dir = repo.display()));
        // The remote may have been repointed in config since the last run;
        // silently reusing a clone of a different repository would deploy the
        // wrong scripts, which is the worst outcome available here.
        let have = git(&["remote", "get-url", "origin"], Some(&repo), log).unwrap_or_default();
        if have != url {
            warn!(log, "{}", tr!("git.origin_changed", from = redact(&have), to = redact(&url)));
            std::fs::remove_dir_all(&repo)
                .map_err(|e| tr!("git.replace", dir = repo.display(), err = e))?;
            clone(&url, &repo, depth, ssh, cred, log)?;
        }
    } else {
        info!(log, "{}", tr!("git.cloning", url = redact(&url), dir = repo.display()));
        clone(&url, &repo, depth, ssh, cred, log)?;
    }

    // Fetched on every run, a fresh clone's too: a clone holds the remote's HEAD
    // alone (`--depth` implies `--single-branch`, and `--no-tags`), so a branch or
    // a tag is not in it until fetched; and a cached clone's own HEAD is the
    // commit the last run deployed, not what the remote has now.
    let target = fetch(&repo, &requested, depth, ssh, cred, log)?;
    checkout(&repo, &target, log)?;
    let commit = git(&["rev-parse", "HEAD"], Some(&repo), log)?;

    // `source_git_dir` is joined, not substituted: the scripts live inside the
    // checkout, and a path escaping it is a configuration error rather than a
    // feature.
    let sub = cfg.get("source_git_dir");
    let sub = sub.trim().trim_start_matches('/');
    if sub.split('/').any(|p| p == "..") {
        return Err(tr!("git.sub_outside", sub = sub));
    }
    let path = if sub.is_empty() { repo.clone() } else { repo.join(sub) };
    if !path.is_dir() {
        return Err(tr!("git.sub_missing", sub = sub, url = url, commit = &commit[..commit.len().min(12)]));
    }

    let src = GitSource {
        path: format!("{}/", path.display()),
        commit,
        requested,
        url,
    };
    info!(log, "{}", tr!("git.source", url = redact(&src.url), commit = src.short(), requested = src.requested));
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

/// Fetch `requested` - "HEAD", a branch, a tag or a sha - and return the commit
/// it names on the remote now.
fn fetch(repo: &Path, requested: &str, depth: u32, ssh: Option<&str>, cred: Option<&str>,
    log: &Logger) -> Result<String, String>
{
    let d = depth.to_string();
    let mut args = vec!["fetch", "--quiet", "--force"];
    if depth > 0 {
        args.push("--depth");
        args.push(d.as_str());
    }
    args.push("origin");
    args.push(requested);
    // FETCH_HEAD is what this fetch just resolved, uniformly for a branch, a tag
    // and a sha; ^{commit} peels an annotated tag to the commit it points at.
    if git_run(&args, Some(repo), ssh, cred, log).is_ok() {
        return git(&["rev-parse", "FETCH_HEAD^{commit}"], Some(repo), log);
    }
    // A shallow clone cannot always fetch an arbitrary commit - servers may
    // refuse a sha that is not a ref tip. Deepening is the documented recovery
    // and costs bandwidth, not correctness, so it is attempted once rather than
    // failing the run.
    warn!(log, "{}", tr!("git.unshallow", requested = requested));
    let _ = git_run(&["fetch", "--quiet", "--unshallow"], Some(repo), ssh, cred, log);
    git_run(&["fetch", "--quiet", "--force", "origin"], Some(repo), ssh, cred, log)?;
    // FETCH_HEAD is the remote's branches now, not what was asked for: only a
    // commit already in the fetched history counts. A ref that is nowhere -
    // a typo - fails here, rather than deploying whatever the remote's default is.
    git(&["rev-parse", "--verify", "--quiet", &format!("{}^{{commit}}", requested)], Some(repo), log)
        .map_err(|_| tr!("git.ref_missing", requested = requested))
}

fn checkout(repo: &Path, target: &str, log: &Logger) -> Result<(), String> {
    // --hard, because the cache is sge's to own: anything modified in it was
    // not put there deliberately, and silently deploying a dirty checkout is
    // how a machine ends up with scripts nobody can point at a revision.
    git(&["reset", "--hard", "--quiet", target], Some(repo), log)?;
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

    #[test]
    fn a_url_with_a_nul_or_no_user_part_is_handled() {
        assert!(sane_url("https://example.com/r.git\0--upload-pack=x").is_err());
        // no scheme: nothing to strip
        assert_eq!(without_userinfo("git@example.com:o/r.git"), "git@example.com:o/r.git");
    }

    // [git] A repository on local disk, reached over file:// - the same clone,
    // fetch and checkout as a remote one, with no network

    fn log() -> Logger {
        Logger::root(slog::Discard, o!())
    }

    /// a scratch directory of the test's own
    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sge-git-test-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// git for a fixture, whatever the user's own configuration says: a fixed
    /// identity, no signing, `main` for the first branch
    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=sge", "-c", "user.email=sge@example.invalid",
                "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false", "-c", "init.defaultBranch=main"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// `file` committed with `body` on the current branch; returns the commit
    fn commit(repo: &Path, file: &str, body: &str) -> String {
        let p = repo.join(file);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        sh(repo, &["add", "."]);
        sh(repo, &["commit", "-q", "-m", file]);
        sh(repo, &["rev-parse", "HEAD"])
    }

    /// the remote: `sql/V1__a.sql` tagged `v1` (annotated), a branch `dev` adding
    /// `sql/V2__dev.sql`, and `main` moved on to `sql/V3__main.sql`. Returns where it
    /// is and its first commit
    fn origin(d: &Path) -> (PathBuf, String) {
        let o = d.join("origin");
        std::fs::create_dir_all(&o).unwrap();
        sh(&o, &["init", "-q"]);
        let first = commit(&o, "sql/V1__a.sql", "select 1;");
        sh(&o, &["tag", "-a", "v1", "-m", "v1"]);
        sh(&o, &["checkout", "-q", "-b", "dev"]);
        commit(&o, "sql/V2__dev.sql", "select 2;");
        sh(&o, &["checkout", "-q", "main"]);
        commit(&o, "sql/V3__main.sql", "select 3;");
        (o, first)
    }

    fn cfg(o: &Path, cache: &Path, extra: &[(&str, &str)]) -> ConfigMap {
        let url = format!("file://{}", o.display());
        let cache = cache.display().to_string();
        let mut pairs = vec![("source_git", url.as_str()), ("source_git_cache", cache.as_str())];
        pairs.extend_from_slice(extra);
        ConfigMap::of(&pairs)
    }

    fn has(g: &GitSource, file: &str) -> bool {
        Path::new(&g.path).join(file).is_file()
    }

    /// [git] no source_git: nothing cloned, the directory source as it was
    #[test]
    fn no_git_source_is_the_directory_source() {
        assert!(materialize(&ConfigMap::of(&[("source_dir", "./sql")]), &log()).unwrap().is_none());
    }

    /// [git] cloned, shallow, at the remote's HEAD - and recorded by its commit
    #[test]
    fn a_repository_is_cloned_at_its_head() {
        let d = scratch("clone");
        let (o, _) = origin(&d);
        let g = materialize(&cfg(&o, &d.join("cache"), &[]), &log()).unwrap().expect("a git source");
        assert_eq!(g.commit, sh(&o, &["rev-parse", "main"]));
        assert_eq!(g.requested, "HEAD");
        assert!(g.path.ends_with('/'), "{}", g.path);
        assert!(has(&g, "sql/V3__main.sql") && !has(&g, "sql/V2__dev.sql"));
        assert_eq!(sh(Path::new(&g.path), &["rev-list", "--count", "HEAD"]), "1", "depth 1 by default");
        remember(&g);
        assert!(revision().is_some());
        let _ = std::fs::remove_dir_all(d);
    }

    /// [git] a later run deploys what the remote has now, not what the cache had -
    /// and a file left in the cache by hand is gone
    #[test]
    fn a_later_run_deploys_what_the_remote_has_now() {
        let d = scratch("update");
        let (o, _) = origin(&d);
        let c = cfg(&o, &d.join("cache"), &[]);
        let g = materialize(&c, &log()).unwrap().unwrap();
        std::fs::write(Path::new(&g.path).join("stray.sql"), "drop table t;").unwrap();
        let next = commit(&o, "sql/V4__later.sql", "select 4;");
        let g = materialize(&c, &log()).unwrap().unwrap();
        assert_eq!(g.commit, next, "the cache's own HEAD is the last run's commit");
        assert!(has(&g, "sql/V4__later.sql"));
        assert!(!has(&g, "stray.sql"), "the cache is sge's: cleaned");
        let _ = std::fs::remove_dir_all(d);
    }

    /// [git] a branch, a tag and a sha each pin the checkout - on the first run,
    /// into a fresh clone that holds the remote's HEAD alone, and on the next
    #[test]
    fn a_branch_a_tag_and_a_sha_pin_the_checkout() {
        let d = scratch("refs");
        let (o, first) = origin(&d);
        let dev = sh(&o, &["rev-parse", "dev"]);
        for (i, (r, want, file)) in [
            ("dev", &dev, "sql/V2__dev.sql"),
            ("v1", &first, "sql/V1__a.sql"),
            (first.as_str(), &first, "sql/V1__a.sql"),
        ].iter().enumerate() {
            let c = cfg(&o, &d.join(format!("cache{}", i)), &[("source_git_ref", r)]);
            for run in ["first", "next"] {
                let g = materialize(&c, &log()).unwrap_or_else(|e| panic!("{} run of {}: {}", run, r, e)).unwrap();
                assert_eq!(&g.commit, *want, "{} run of {}", run, r);
                assert_eq!(g.requested, *r);
                assert!(has(&g, file), "{} run of {}: no {}", run, r, file);
                assert!(!has(&g, "sql/V3__main.sql"), "{} run of {}: main's file", run, r);
            }
        }
        let _ = std::fs::remove_dir_all(d);
    }

    /// [git] a ref the remote will not fetch alone - an abbreviated sha - is found
    /// in the full history: the clone is deepened once
    #[test]
    fn a_short_sha_is_found_by_deepening_the_clone() {
        let d = scratch("short");
        let (o, first) = origin(&d);
        let c = cfg(&o, &d.join("cache"), &[("source_git_ref", &first[..10])]);
        let g = materialize(&c, &log()).unwrap();
        assert_eq!(g.unwrap().commit, first);
        let _ = std::fs::remove_dir_all(d);
    }

    /// [git] a ref that is nowhere - a typo - fails the run, on a fresh clone and on
    /// a cached one; it never deploys the remote's default branch instead
    #[test]
    fn a_ref_that_is_nowhere_fails_the_run() {
        let d = scratch("typo");
        let (o, _) = origin(&d);
        let cache = d.join("cache");
        let typo = cfg(&o, &cache, &[("source_git_ref", "mian")]);
        let e = materialize(&typo, &log()).expect_err("a fresh clone: refused");
        assert!(e.starts_with("SGE-E511 ") && e.contains("'mian'"), "{}", e);
        materialize(&cfg(&o, &cache, &[]), &log()).unwrap().unwrap();
        let e = materialize(&typo, &log()).expect_err("a cached clone: refused");
        assert!(e.starts_with("SGE-E511 ") && e.contains("'mian'"), "{}", e);
        let _ = std::fs::remove_dir_all(d);
    }

    /// [git] a cache whose origin is another repository is cloned again, never reused
    #[test]
    fn a_cache_of_another_origin_is_cloned_again() {
        let d = scratch("origin");
        let (o, _) = origin(&d);
        let c = cfg(&o, &d.join("cache"), &[]);
        let g = materialize(&c, &log()).unwrap().unwrap();
        sh(Path::new(&g.path), &["remote", "set-url", "origin", "file:///elsewhere.git"]);
        let g = materialize(&c, &log()).unwrap().unwrap();
        assert_eq!(sh(Path::new(&g.path), &["remote", "get-url", "origin"]), format!("file://{}", o.display()));
        assert_eq!(g.commit, sh(&o, &["rev-parse", "main"]));
        let _ = std::fs::remove_dir_all(d);
    }

    /// [git] source_git_dir: the scripts from a sub-directory of the checkout; one
    /// that is not there, or climbs out of it, refused
    #[test]
    fn the_scripts_can_sit_in_a_sub_directory() {
        let d = scratch("subdir");
        let (o, _) = origin(&d);
        let cache = d.join("cache");
        let g = materialize(&cfg(&o, &cache, &[("source_git_dir", "/sql")]), &log()).unwrap().unwrap();
        assert!(g.path.ends_with("/sql/"), "{}", g.path);
        assert!(has(&g, "V3__main.sql"));
        let e = materialize(&cfg(&o, &cache, &[("source_git_dir", "sql/../..")]), &log()).err().unwrap();
        assert!(e.starts_with("SGE-E509 "), "{}", e);
        let e = materialize(&cfg(&o, &cache, &[("source_git_dir", "nope")]), &log()).err().unwrap();
        assert!(e.starts_with("SGE-E510 "), "{}", e);
        let _ = std::fs::remove_dir_all(d);
    }

    /// [git] a credential file reaches git as a path; one with whitespace - which
    /// git would split into two arguments - is refused
    #[test]
    fn a_credential_file_is_passed_by_path() {
        let d = scratch("cred");
        let (o, _) = origin(&d);
        let cache = d.join("cache");
        let file = d.join("credentials").display().to_string();
        assert!(materialize(&cfg(&o, &cache, &[("source_git_credentials", &file)]), &log()).unwrap().is_some());
        let e = materialize(&cfg(&o, &cache, &[("source_git_credentials", "/a b")]), &log()).err().unwrap();
        assert!(e.starts_with("SGE-E504 "), "{}", e);
        let e = materialize(&cfg(&o, &cache, &[("source_git_ssh_key", "/k'x")]), &log()).err().unwrap();
        assert!(e.starts_with("SGE-E503 "), "{}", e);
        let _ = std::fs::remove_dir_all(d);
    }

    /// [git] a repository that is not there fails the run with git's own reason
    #[test]
    fn a_missing_repository_fails_with_gits_reason() {
        let d = scratch("missing");
        let c = cfg(&d.join("nothing-here"), &d.join("cache"), &[]);
        let e = materialize(&c, &log()).expect_err("refused");
        assert!(e.starts_with("SGE-E506 ") && e.contains("git clone"), "{}", e);
        let _ = std::fs::remove_dir_all(d);
    }
}
