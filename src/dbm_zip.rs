//! [zip] A zip archive as a script source - and the shape a blob or S3 source takes.
//!
//! `source_zip` names an archive. A `migrate` or `prepare` run unpacks it into
//! `<tmp>/<archive name>/` - `/tmp/scripts.zip/` for `scripts.zip` - and reads its
//! scripts from there: `source_dir` is replaced for the run, joined with
//! `source_zip_dir` when the scripts sit in a sub-directory of the archive. The
//! directory is removed when the run ends, completed or failed. As for a git source,
//! everything downstream - filters, version parsing, checksums, history - reads a
//! directory on disk and cannot tell where it came from, so an archive cannot change
//! which scripts run or how they are recorded.
//!
//! What it deployed is recorded: the history rows are tagged `zip:<name>@<md5>`
//! (twelve hex digits of the archive's MD5) unless `script_tag` is given, the way a
//! git source tags them with its commit.
//!
//! ## Configuration
//!
//! | key | meaning |
//! |---|---|
//! | `source_zip` | path of the archive. Empty (the default) keeps `source_dir` as it is. |
//! | `source_zip_dir` | sub-directory inside the archive holding the scripts. |
//! | `source_zip_tmp` | where archives are unpacked. Default: the system temp directory. |
//! | `source_zip_max_mb` | the most it unpacks, in MiB; default 1024. |
//!
//! ## Safety
//!
//! - an entry that would land outside the directory - `../`, an absolute path - is
//!   refused, and the archive with it (zip slip); so is a symlink;
//! - `source_zip_max_mb` caps what is written, against an archive that inflates
//!   without end;
//! - the directory is this run's own. One that is there already belongs to another
//!   run - refused, never removed - unless the run that made it is gone: its pid
//!   file (`<archive name>.pid` beside it) names a process that no longer exists,
//!   which is what a killed run leaves. Then it is removed and unpacked again.
//!
//! The release build aborts on a panic, with no unwinding: a panic leaves the
//! directory, as a kill does, and the next run clears it the same way.
//!
//! ## Next: blob and S3 storage
//!
//! A fetched source is one step in front of the directory: put the scripts on local
//! disk, read them as a directory, remove them when the run ends. A zip has the
//! archive on disk already; a blob or an S3 source adds only the fetch - an object
//! (`s3://bucket/key.zip`, an `https://` URL to a blob) downloaded into
//! `<tmp>/<name>` and then handled exactly as `source_zip` is, the download removed
//! with the directory. The claim, the pid file, the unpacking, the tag and the
//! clean-up below stay as they are; credentials come from the environment the
//! platform's own tools read, as git's do, never from the command line.

use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use md5::{Digest, Md5};
use slog::Logger;

use crate::dbm_cfg::ConfigMap;

const MAX_MB_DEFAULT: u64 = 1024;

/// An unpacked archive: where its scripts are, and what it was. Removed on drop -
/// when the run ends, whichever way.
pub(crate) struct ZipSource {
    /// the directory the scripts are read from: `dir` joined with `source_zip_dir`
    pub(crate) path: String,
    /// `zip:<archive name>@<md5, 12 hex digits>`
    pub(crate) tag: String,
    /// the run's own directory, `<tmp>/<archive name>`
    dir: PathBuf,
    pid: PathBuf,
    log: Logger,
}

impl Drop for ZipSource {
    fn drop(&mut self) {
        match fs::remove_dir_all(&self.dir) {
            Ok(()) => info!(self.log, "{}", tr!("zip.removed", dir = self.dir.display())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => warn!(self.log, "{}", tr!("zip.not_removed", dir = self.dir.display(), err = e)),
        }
        let _ = fs::remove_file(&self.pid);
    }
}

/// Unpack `source_zip` and point `source_dir` at it for the run. `Ok(None)` when no
/// archive is configured, which leaves the configuration as it was.
pub(crate) fn materialize(cfg: &mut ConfigMap, log: &Logger) -> Result<Option<ZipSource>, String> {
    let archive = cfg.get("source_zip");
    let archive = archive.trim();
    if archive.is_empty() {
        return Ok(None);
    }
    if !cfg.get("source_git").trim().is_empty() {
        return Err(tr!("zip.both_sources"));
    }
    let archive = PathBuf::from(archive);
    if !archive.is_file() {
        return Err(tr!("zip.no_file", archive = archive.display()));
    }
    let name = archive.file_name().map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty() && !n.starts_with('.'))
        .ok_or_else(|| tr!("zip.no_name", archive = archive.display()))?;
    let sub = sub_dir(&cfg.get("source_zip_dir"))?;
    let max = match cfg.get("source_zip_max_mb").trim() {
        "" => MAX_MB_DEFAULT,
        s => s.parse::<u64>().map_err(|_| tr!("zip.max_mb", value = s))?,
    } * 1024 * 1024;
    let root = match cfg.get("source_zip_tmp").trim() {
        "" => std::env::temp_dir(),
        s => PathBuf::from(s),
    };
    fs::create_dir_all(&root).map_err(|e| tr!("zip.tmp", dir = root.display(), err = e))?;

    let dir = root.join(&name);
    let pid = root.join(format!("{}.pid", name));
    claim(&dir, &pid, log)?;
    // from here the directory is this run's: dropped, the guard removes it - an
    // unpacking that fails half way included
    let mut src = ZipSource { path: String::new(), tag: String::new(), dir, pid, log: log.clone() };
    fs::write(&src.pid, std::process::id().to_string())
        .map_err(|e| tr!("zip.pid", file = src.pid.display(), err = e))?;

    let files = unpack(&archive, &src.dir, max)?;
    let scripts = match &sub {
        None => src.dir.clone(),
        Some(s) => src.dir.join(s),
    };
    if !scripts.is_dir() {
        return Err(tr!("zip.sub_missing", sub = sub.unwrap_or_default().display(), archive = archive.display()));
    }
    src.tag = format!("zip:{}@{}", name, md5_hex(&archive)?.get(..12).unwrap_or_default());
    let mut path = scripts.display().to_string();
    if !path.ends_with('/') {
        path.push('/');
    }
    src.path = path;
    info!(log, "{}", tr!("zip.unpacked", archive = archive.display(), dir = src.dir.display(), files = files,
        path = src.path, tag = src.tag));
    cfg.insert(&"source_dir".to_string(), src.path.as_str());
    crate::dbm_git::remember_tag(&src.tag);
    Ok(Some(src))
}

/// Make `dir` this run's: created new, or - when it is there - taken over only from a
/// run that is gone.
fn claim(dir: &Path, pid: &Path, log: &Logger) -> Result<(), String> {
    match fs::create_dir(dir) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(tr!("zip.dir", dir = dir.display(), err = e)),
    }
    let owner = fs::read_to_string(pid).ok().and_then(|s| s.trim().parse::<u32>().ok());
    match owner {
        Some(p) if !alive(p) => {
            warn!(log, "{}", tr!("zip.stale", dir = dir.display(), pid = p));
            fs::remove_dir_all(dir).map_err(|e| tr!("zip.dir", dir = dir.display(), err = e))?;
            let _ = fs::remove_file(pid);
            fs::create_dir(dir).map_err(|e| tr!("zip.dir", dir = dir.display(), err = e))
        }
        Some(p) => Err(tr!("zip.in_use", dir = dir.display(), pid = p)),
        None => Err(tr!("zip.unclaimed", dir = dir.display(), pid_file = pid.display())),
    }
}

/// Is process `pid` running? Read from /proc where there is one; elsewhere every
/// process is taken to be alive, so a directory is never removed on a guess.
fn alive(pid: u32) -> bool {
    let proc_root = Path::new("/proc");
    if !proc_root.join("self").exists() {
        return true;
    }
    proc_root.join(pid.to_string()).exists()
}

/// `source_zip_dir` as a relative path inside the archive: nothing that climbs out of it
fn sub_dir(s: &str) -> Result<Option<PathBuf>, String> {
    let s = s.trim().trim_matches('/');
    if s.is_empty() {
        return Ok(None);
    }
    let p = PathBuf::from(s);
    if p.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(tr!("zip.sub_outside", sub = s));
    }
    Ok(Some(p))
}

/// Unpack `archive` into `into`, which exists. Returns how many files it wrote.
fn unpack(archive: &Path, into: &Path, max: u64) -> Result<usize, String> {
    let err = |e: &dyn std::fmt::Display| tr!("zip.archive", archive = archive.display(), err = e);
    let f = fs::File::open(archive).map_err(|e| err(&e))?;
    let mut zip = zip::ZipArchive::new(f).map_err(|e| err(&e))?;
    let (mut total, mut files) = (0u64, 0usize);
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| err(&e))?;
        let shown = entry.name().map_err(|e| err(&e))?.into_owned();
        let rel = entry.enclosed_name()
            .ok_or_else(|| tr!("zip.entry_outside", archive = archive.display(), entry = shown))?;
        if entry.is_symlink() {
            return Err(tr!("zip.entry_symlink", archive = archive.display(), entry = shown));
        }
        let out = into.join(&rel);
        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(|e| err(&e))?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(|e| err(&e))?;
        }
        let mut w = fs::File::create(&out).map_err(|e| err(&e))?;
        // one byte past what is left: reaching it says the cap was passed
        let left = max.saturating_sub(total);
        let n = io::copy(&mut (&mut entry).take(left + 1), &mut w).map_err(|e| err(&e))?;
        total += n;
        if total > max {
            return Err(tr!("zip.too_big", archive = archive.display(), mb = max / 1024 / 1024));
        }
        files += 1;
    }
    Ok(files)
}

fn md5_hex(path: &Path) -> Result<String, String> {
    let mut f = fs::File::open(path).map_err(|e| tr!("zip.read", file = path.display(), err = e))?;
    let mut h = Md5::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| tr!("zip.read", file = path.display(), err = e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{:02x}", b)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn log() -> Logger {
        Logger::root(slog::Discard, o!())
    }

    /// a scratch directory of the test's own
    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sge-zip-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// an archive of `(name, bytes)` entries, deflated
    fn archive(at: &Path, entries: &[(&str, &[u8])]) -> PathBuf {
        let path = at.join("scripts.zip");
        let mut z = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        let opt = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            z.start_file(*name, opt).unwrap();
            z.write_all(bytes).unwrap();
        }
        z.finish().unwrap();
        path
    }

    fn cfg(archive: &Path, tmp: &Path, extra: &[(&str, &str)]) -> ConfigMap {
        let mut pairs = vec![("source_zip", archive.to_str().unwrap()), ("source_zip_tmp", tmp.to_str().unwrap())];
        pairs.extend_from_slice(extra);
        ConfigMap::of(&pairs)
    }

    /// [zip] the scripts read from `<tmp>/<archive name>/`, source_dir pointed there,
    /// the run tagged by the archive - and the directory gone when the run ends
    #[test]
    fn a_zip_is_unpacked_for_the_run_and_removed_after() {
        let d = scratch("run");
        let a = archive(&d, &[("V1_0__init.sql", b"create table t(i int);"), ("sql/R1__view.sql", b"select 1;")]);
        let tmp = d.join("tmp");
        let mut c = cfg(&a, &tmp, &[]);
        let src = materialize(&mut c, &log()).unwrap().expect("an archive is configured");
        let dir = tmp.join("scripts.zip");
        assert_eq!(src.path, format!("{}/", dir.display()));
        assert_eq!(c.get("source_dir"), src.path, "source_dir is the unpacked directory for the run");
        assert_eq!(fs::read(dir.join("V1_0__init.sql")).unwrap(), b"create table t(i int);");
        assert_eq!(fs::read(dir.join("sql/R1__view.sql")).unwrap(), b"select 1;");
        assert!(src.tag.starts_with("zip:scripts.zip@") && src.tag.len() == "zip:scripts.zip@".len() + 12, "{}", src.tag);
        assert_eq!(src.tag[16..], md5_hex(&a).unwrap()[..12], "the archive's own md5");
        assert!(tmp.join("scripts.zip.pid").is_file());
        drop(src);
        assert!(!dir.exists(), "removed when the run ends");
        assert!(!tmp.join("scripts.zip.pid").exists());
        let _ = fs::remove_dir_all(d);
    }

    /// [zip] no archive configured: nothing unpacked, source_dir as it was
    #[test]
    fn no_zip_leaves_source_dir_alone() {
        let mut c = ConfigMap::of(&[("source_dir", "./sql")]);
        assert!(materialize(&mut c, &log()).unwrap().is_none());
        assert_eq!(c.get("source_dir"), "./sql");
    }

    /// [zip] source_zip_dir: the scripts from a sub-directory of the archive; one that
    /// is not in it, or climbs out of it, refused - and nothing left behind
    #[test]
    fn the_scripts_can_sit_in_a_sub_directory() {
        let d = scratch("sub");
        let a = archive(&d, &[("db/migrations/V1__a.sql", b"select 1;")]);
        let tmp = d.join("tmp");
        let mut c = cfg(&a, &tmp, &[("source_zip_dir", "db/migrations")]);
        let src = materialize(&mut c, &log()).unwrap().unwrap();
        assert_eq!(src.path, format!("{}/", tmp.join("scripts.zip/db/migrations").display()));
        drop(src);
        let mut c = cfg(&a, &tmp, &[("source_zip_dir", "nope")]);
        assert!(materialize(&mut c, &log()).err().unwrap().starts_with("SGE-E607 "));
        assert!(!tmp.join("scripts.zip").exists(), "a failed run removes its directory too");
        let mut c = cfg(&a, &tmp, &[("source_zip_dir", "../etc")]);
        assert!(materialize(&mut c, &log()).err().unwrap().starts_with("SGE-E611 "));
        let _ = fs::remove_dir_all(d);
    }

    /// [zip] an entry that would land outside the directory refuses the archive, and
    /// the half-unpacked directory goes with the failed run
    #[test]
    fn an_entry_outside_the_directory_refuses_the_archive() {
        let d = scratch("slip");
        let a = archive(&d, &[("V1__ok.sql", b"select 1;"), ("../../evil.sql", b"drop table t;")]);
        let tmp = d.join("tmp");
        let mut c = cfg(&a, &tmp, &[]);
        let e = materialize(&mut c, &log()).err().expect("refused");
        assert!(e.starts_with("SGE-E613 ") && e.contains("evil.sql"), "{}", e);
        assert!(!tmp.join("scripts.zip").exists(), "removed after the failure");
        assert!(!d.join("evil.sql").exists() && !tmp.join("evil.sql").exists());
        let _ = fs::remove_dir_all(d);
    }

    /// [zip] past source_zip_max_mb the archive is refused, its directory removed
    #[test]
    fn an_archive_past_the_cap_is_refused() {
        let d = scratch("cap");
        let big = vec![0u8; 2 * 1024 * 1024];
        let a = archive(&d, &[("V1__big.sql", big.as_slice())]);
        let tmp = d.join("tmp");
        let mut c = cfg(&a, &tmp, &[("source_zip_max_mb", "1")]);
        let e = materialize(&mut c, &log()).err().expect("refused");
        assert!(e.starts_with("SGE-E615 "), "{}", e);
        assert!(!tmp.join("scripts.zip").exists());
        let _ = fs::remove_dir_all(d);
    }

    /// [zip] a directory another live run holds is refused and kept; one a gone run
    /// left is taken over
    #[test]
    fn a_directory_in_use_is_refused_and_a_stale_one_taken_over() {
        let d = scratch("claim");
        let a = archive(&d, &[("V1__a.sql", b"select 1;")]);
        let tmp = d.join("tmp");
        fs::create_dir_all(tmp.join("scripts.zip")).unwrap();
        // this test's own process: alive
        fs::write(tmp.join("scripts.zip.pid"), std::process::id().to_string()).unwrap();
        let mut c = cfg(&a, &tmp, &[]);
        let e = materialize(&mut c, &log()).err().expect("in use");
        assert!(e.starts_with("SGE-E609 "), "{}", e);
        assert!(tmp.join("scripts.zip").is_dir(), "another run's directory is never removed");
        if Path::new("/proc/self").exists() {
            // a pid no process has: the run that left it is gone
            fs::write(tmp.join("scripts.zip.pid"), u32::MAX.to_string()).unwrap();
            let src = materialize(&mut c, &log()).unwrap().unwrap();
            assert!(tmp.join("scripts.zip/V1__a.sql").is_file());
            drop(src);
        }
        let _ = fs::remove_dir_all(d);
    }

    /// [zip] one script source a run: an archive and a repository together refused
    #[test]
    fn zip_and_git_together_are_refused() {
        let d = scratch("both");
        let a = archive(&d, &[("V1__a.sql", b"select 1;")]);
        let mut c = cfg(&a, &d.join("tmp"), &[("source_git", "https://example.invalid/r.git")]);
        assert!(materialize(&mut c, &log()).err().unwrap().starts_with("SGE-E601 "));
        let _ = fs::remove_dir_all(d);
    }
}
