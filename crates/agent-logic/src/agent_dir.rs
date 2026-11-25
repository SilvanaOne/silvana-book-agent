//! The agent folder: the directory of the env file the agent loaded, else the current
//! directory. It holds the agent's local state (`control/`, `usdcx/`).

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};

/// The file a moved folder names its old location in.
pub const MOVED_FROM_FILE: &str = "moved_from.json";
/// Suffix of the copy a move builds before renaming it into place.
pub const MOVING_SUFFIX: &str = ".migrating";
/// Longest wait for the locks a move takes.
pub const MOVE_LOCK_WAIT: Duration = Duration::from_secs(30);
const LOCK_POLL: Duration = Duration::from_millis(100);
/// Largest file a move copies (64 MiB).
const MAX_COPY_BYTES: u64 = 67_108_864;
/// Deepest folder nesting a move copies.
const MAX_COPY_DEPTH: usize = 8;

static AGENT_DIR: OnceLock<PathBuf> = OnceLock::new();

/// The agent folder of `env_file`: the directory of the file a write to it replaces (a
/// symlink's target), else the current directory.
pub fn of_env_file(env_file: Option<&Path>) -> Result<PathBuf> {
    let file = env_file
        .map(crate::control_store::write_target)
        .transpose()?;
    match file
        .as_deref()
        .and_then(Path::parent)
        .filter(|dir| !dir.as_os_str().is_empty())
    {
        Some(dir) => std::path::absolute(dir).with_context(|| format!("resolve {}", dir.display())),
        None => std::env::current_dir().context("cannot read the current directory"),
    }
}

/// Records the agent folder of this process, once, at startup.
pub fn set(dir: PathBuf) -> Result<()> {
    match AGENT_DIR.set(dir) {
        Ok(()) => Ok(()),
        Err(dir) if AGENT_DIR.get() == Some(&dir) => Ok(()),
        Err(dir) => bail!("the agent folder is already set, not to {}", dir.display()),
    }
}

/// The agent folder recorded at startup, else the current directory.
pub fn current() -> Option<PathBuf> {
    AGENT_DIR
        .get()
        .cloned()
        .or_else(|| std::env::current_dir().ok())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MovedFrom {
    from: String,
    moved_at: i64,
}

/// Moves `legacy` to a missing `target` once: a copy checked by `verify` is renamed into place,
/// and `legacy` is only read. A `target` beside `legacy` must name it in [`MOVED_FROM_FILE`].
pub fn move_once(
    what: &str,
    legacy: &Path,
    target: &Path,
    legacy_locks: &[&str],
    verify: &dyn Fn(&Path) -> Result<()>,
) -> Result<bool> {
    if !is_dir(legacy)? {
        return Ok(false);
    }
    if exists(target)? {
        check_moved(what, legacy, target)?;
        return Ok(false);
    }
    ensure!(
        !resolved(target).starts_with(resolved(legacy)),
        "refusing to move the {what} from {} into {}, which lies inside it: choose a folder outside it",
        legacy.display(),
        target.display()
    );
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| anyhow!("{} has no parent folder", target.display()))?;
    let name = target
        .file_name()
        .ok_or_else(|| anyhow!("{} names no folder", target.display()))?;
    create_private_dir(parent)?;
    let deadline = Instant::now().checked_add(MOVE_LOCK_WAIT);
    let _folder = lock_folder(parent, deadline)?;
    if exists(target)? {
        check_moved(what, legacy, target)?;
        return Ok(false);
    }
    let mut held = Vec::new();
    for lock in legacy_locks {
        if let Some(handle) = lock_existing(&legacy.join(lock), deadline)? {
            held.push(handle);
        }
    }
    let mut copy_name = name.to_os_string();
    copy_name.push(MOVING_SUFFIX);
    let copy = parent.join(copy_name);
    remove_tree(&copy)?;
    let built = copy_tree(legacy, &copy, 0)
        .and_then(|()| verify(&copy))
        .and_then(|()| record_source(&copy, legacy));
    if let Err(e) = built {
        let _ = remove_tree(&copy);
        return Err(e.context(format!(
            "refusing to move the {what} from {} to {}",
            legacy.display(),
            target.display()
        )));
    }
    if let Err(e) = fs::rename(&copy, target) {
        let _ = remove_tree(&copy);
        return Err(e).with_context(|| format!("rename {} to {}", copy.display(), target.display()));
    }
    sync_dir(parent).with_context(|| format!("sync {}", parent.display()))?;
    drop(held);
    tracing::info!(
        "moved the {what} from {} to {} (the old folder is left as it was)",
        legacy.display(),
        target.display()
    );
    Ok(true)
}

/// The old location `dir` names in its [`MOVED_FROM_FILE`], if any.
pub fn moved_from(dir: &Path) -> Result<Option<String>> {
    let path = dir.join(MOVED_FROM_FILE);
    let bytes = match read_bounded(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == ErrorKind::NotFound) => {
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    let moved: MovedFrom = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not a valid record of a move", path.display()))?;
    Ok(Some(moved.from))
}

/// `target` beside `legacy` is fine only when it is `legacy` itself or was moved from it.
fn check_moved(what: &str, legacy: &Path, target: &Path) -> Result<()> {
    if same_dir(legacy, target) {
        return Ok(());
    }
    match moved_from(target)? {
        Some(from) if from == legacy.to_string_lossy() => Ok(()),
        Some(from) => bail!(
            "refusing to use {}: it holds the {what} moved from {from}, but {} exists too; give \
             each agent its own folder, or keep the right copy and move the other out",
            target.display(),
            legacy.display()
        ),
        None => bail!(
            "refusing to choose between two copies of the {what}: {} and {} both exist; keep the \
             right one, move the other out of both places, then retry",
            target.display(),
            legacy.display()
        ),
    }
}

/// Whether both paths name one folder.
fn same_dir(a: &Path, b: &Path) -> bool {
    matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

/// `path` made absolute, with its longest existing prefix resolved through symlinks.
fn resolved(path: &Path) -> PathBuf {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut rest = Vec::new();
    let mut prefix = path.as_path();
    loop {
        if let Ok(real) = fs::canonicalize(prefix) {
            return rest.iter().rev().fold(real, |acc, name| acc.join(name));
        }
        match (prefix.parent(), prefix.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                prefix = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

fn is_dir(path: &Path) -> Result<bool> {
    match fs::metadata(path) {
        Ok(meta) => Ok(meta.is_dir()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

fn wait_lock(handle: File, path: &Path, deadline: Option<Instant>) -> Result<File> {
    loop {
        match handle.try_lock() {
            Ok(()) => return Ok(handle),
            Err(fs::TryLockError::WouldBlock) => {
                if deadline.is_none_or(|d| Instant::now() >= d) {
                    bail!(
                        "another process holds {} — retry when it finishes",
                        path.display()
                    );
                }
                std::thread::sleep(LOCK_POLL);
            }
            Err(fs::TryLockError::Error(e)) => {
                return Err(e).with_context(|| format!("lock {}", path.display()));
            }
        }
    }
}

/// An exclusive lock on the folder itself; it creates nothing.
#[cfg(unix)]
fn lock_folder(dir: &Path, deadline: Option<Instant>) -> Result<File> {
    let handle = File::open(dir).with_context(|| format!("open {}", dir.display()))?;
    wait_lock(handle, dir, deadline)
}

#[cfg(not(unix))]
fn lock_folder(dir: &Path, deadline: Option<Instant>) -> Result<File> {
    let path = dir.join(".move.lock");
    let handle = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    wait_lock(handle, &path, deadline)
}

/// Locks `path` when it exists, opened read-only; a missing file is not created.
fn lock_existing(path: &Path, deadline: Option<Instant>) -> Result<Option<File>> {
    match File::open(path) {
        Ok(handle) => wait_lock(handle, path, deadline).map(Some),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("open {}", path.display())),
    }
}

/// Lock files and leftover temp files are not state.
fn skipped(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name.ends_with(".lock") || name.ends_with(".tmp")
}

fn copy_tree(from: &Path, to: &Path, depth: usize) -> Result<()> {
    ensure!(
        depth <= MAX_COPY_DEPTH,
        "{} is nested too deep to copy",
        from.display()
    );
    create_private_dir(to)?;
    let listing = fs::read_dir(from).with_context(|| format!("read {}", from.display()))?;
    for item in listing {
        let item = item.with_context(|| format!("read {}", from.display()))?;
        let name = item.file_name();
        if skipped(&name) {
            continue;
        }
        let source = item.path();
        let meta = fs::metadata(&source).with_context(|| format!("read {}", source.display()))?;
        let dest = to.join(&name);
        if meta.is_dir() {
            copy_tree(&source, &dest, depth.saturating_add(1))?;
        } else if meta.is_file() {
            let bytes = read_bounded(&source)?;
            write_new(&dest, &bytes)?;
        } else {
            bail!("{} is neither a file nor a folder", source.display());
        }
    }
    sync_dir(to).with_context(|| format!("sync {}", to.display()))
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let file = File::open(path).with_context(|| format!("read {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_COPY_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {}", path.display()))?;
    ensure!(
        u64::try_from(bytes.len()).is_ok_and(|n| n <= MAX_COPY_BYTES),
        "{} is too large to copy",
        path.display()
    );
    Ok(bytes)
}

/// Writes a new file, mode 0600, and syncs it.
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("write {}", path.display()))
}

fn record_source(copy: &Path, legacy: &Path) -> Result<()> {
    let record = MovedFrom {
        from: legacy.to_string_lossy().into_owned(),
        moved_at: crate::clock::now_secs_i64(),
    };
    write_new(
        &copy.join(MOVED_FROM_FILE),
        &serde_json::to_vec_pretty(&record)?,
    )?;
    sync_dir(copy).with_context(|| format!("sync {}", copy.display()))
}

/// Removes our own leftover copy: a folder, or anything else at that path.
fn remove_tree(path: &Path) -> Result<()> {
    let removed = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => Err(e),
    };
    removed.with_context(|| format!("remove {}", path.display()))
}

fn create_private_dir(dir: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .with_context(|| format!("create {}", dir.display()))
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    match File::open(dir).and_then(|d| d.sync_all()) {
        // Some filesystems cannot sync a directory
        Err(e) if matches!(e.kind(), ErrorKind::InvalidInput | ErrorKind::Unsupported) => Ok(()),
        other => other,
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut bytes = [0u8; 8];
        let _ = rand::RngCore::try_fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
        let dir = std::env::temp_dir().join(format!("agent-dir-{tag}-{}", hex::encode(bytes)));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Every file under `dir` with its bytes, sorted.
    fn tree(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(next) = stack.pop() {
            for item in fs::read_dir(&next).unwrap() {
                let path = item.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let bytes = fs::read(&path).unwrap();
                    out.push((path.strip_prefix(dir).unwrap().to_path_buf(), bytes));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn the_agent_folder_is_the_env_files_directory_else_the_current_one() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(of_env_file(Some(Path::new("/srv/agent-1/.env"))).unwrap(), PathBuf::from("/srv/agent-1"));
        assert_eq!(of_env_file(Some(Path::new("agent-2/.env"))).unwrap(), cwd.join("agent-2"));
        assert_eq!(of_env_file(Some(Path::new(".env"))).unwrap(), cwd);
        assert_eq!(of_env_file(None).unwrap(), cwd);
        assert!(current().is_some());
    }

    #[test]
    fn a_move_copies_skips_locks_records_its_source_and_leaves_the_old_folder() {
        let root = temp_dir("move");
        let legacy = root.join("old").join("state");
        fs::create_dir_all(legacy.join("sub")).unwrap();
        fs::write(legacy.join("a.json"), b"{}").unwrap();
        fs::write(legacy.join("sub").join("b.json"), b"[1]").unwrap();
        fs::write(legacy.join("a.json.lock"), b"").unwrap();
        fs::write(legacy.join(".a.json.1.2.tmp"), b"x").unwrap();
        let before = tree(&legacy);
        let target = root.join("agent").join("state");
        // A stale copy of an earlier attempt is replaced.
        fs::create_dir_all(root.join("agent").join("state.migrating")).unwrap();
        fs::write(root.join("agent").join("state.migrating").join("junk"), b"x").unwrap();
        assert!(move_once("state", &legacy, &target, &["a.json.lock"], &|_| Ok(())).unwrap());
        assert_eq!(tree(&legacy), before, "the old folder is unchanged");
        assert!(!root.join("agent").join("state.migrating").exists());
        let moved: Vec<PathBuf> = tree(&target).into_iter().map(|(p, _)| p).collect();
        assert_eq!(moved, [PathBuf::from("a.json"), PathBuf::from(MOVED_FROM_FILE), PathBuf::from("sub/b.json")]);
        assert_eq!(fs::read(target.join("sub").join("b.json")).unwrap(), b"[1]");
        assert_eq!(moved_from(&target).unwrap().as_deref(), Some(&*legacy.to_string_lossy()));
        // Once moved, the old folder beside it is the stale copy: nothing more happens.
        assert!(!move_once("state", &legacy, &target, &[], &|_| Ok(())).unwrap());
        // No old folder: nothing to move.
        assert!(!move_once("state", &root.join("none"), &root.join("fresh"), &[], &|_| Ok(())).unwrap());
        assert!(!root.join("fresh").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn two_copies_or_a_failed_check_are_refused() {
        let root = temp_dir("refuse");
        let legacy = root.join("old");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("a.json"), b"{}").unwrap();
        let target = root.join("agent").join("state");
        fs::create_dir_all(&target).unwrap();
        let err = format!("{:#}", move_once("state", &legacy, &target, &[], &|_| Ok(())).unwrap_err());
        assert!(err.contains(&*target.to_string_lossy()) && err.contains(&*legacy.to_string_lossy()), "{err}");
        fs::remove_dir_all(&target).unwrap();
        let err = move_once("state", &legacy, &target, &[], &|copy| {
            assert!(copy.ends_with("state.migrating") && copy.join("a.json").exists());
            bail!("bad copy")
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("bad copy"), "{err:#}");
        assert!(!target.exists() && !root.join("agent").join("state.migrating").exists());
        assert!(legacy.join("a.json").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_env_file_puts_the_folder_where_its_writes_go() {
        let root = temp_dir("symlink");
        let (real, links) = (root.join("real"), root.join("links"));
        fs::create_dir_all(&real).unwrap();
        fs::create_dir_all(&links).unwrap();
        fs::write(real.join(".env"), b"A=1\n").unwrap();
        let link = links.join("agent.env");
        std::os::unix::fs::symlink(real.join(".env"), &link).unwrap();
        // The marker is written through the link, into the real file.
        crate::control_store::write_atomic(&link, b"A=2\n", 0o600).unwrap();
        assert_eq!(fs::read(real.join(".env")).unwrap(), b"A=2\n");
        assert_eq!(of_env_file(Some(&link)).unwrap(), fs::canonicalize(&real).unwrap());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_old_folder_itself_is_not_a_second_copy() {
        let root = temp_dir("same");
        let legacy = root.join("old").join("state");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("a.json"), b"{}").unwrap();
        let before = tree(&legacy);
        assert!(!move_once("state", &legacy, &legacy, &[], &|_| Ok(())).unwrap());
        #[cfg(unix)]
        {
            let link = root.join("agent");
            fs::create_dir_all(&link).unwrap();
            std::os::unix::fs::symlink(&legacy, link.join("state")).unwrap();
            assert!(!move_once("state", &legacy, &link.join("state"), &[], &|_| Ok(())).unwrap());
        }
        assert_eq!(tree(&legacy), before);
        assert!(!root.join("old").join("state.migrating").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_target_inside_the_old_folder_is_refused_before_anything_is_written() {
        let root = temp_dir("nest");
        let legacy = root.join("old").join("control");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("a.json"), b"{}").unwrap();
        let before = tree(&legacy);
        for target in [legacy.join("control"), legacy.join("sub").join("control")] {
            let err = move_once("state", &legacy, &target, &[], &|_| bail!("nothing is copied")).unwrap_err();
            assert!(format!("{err:#}").contains("which lies inside it"), "{err:#}");
        }
        assert_eq!(tree(&legacy), before);
        assert_eq!(fs::read_dir(&legacy).unwrap().count(), 1, "nothing was created in the old folder");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_moved_from_another_place_is_refused_naming_it() {
        let root = temp_dir("other");
        let (a, b) = (root.join("a"), root.join("b"));
        for dir in [&a, &b] {
            fs::create_dir_all(dir).unwrap();
            fs::write(dir.join("x.json"), b"{}").unwrap();
        }
        let target = root.join("agent").join("state");
        assert!(move_once("state", &a, &target, &[], &|_| Ok(())).unwrap());
        let err = format!("{:#}", move_once("state", &b, &target, &[], &|_| Ok(())).unwrap_err());
        assert!(err.contains(&*a.to_string_lossy()) && err.contains("its own folder"), "{err}");
        assert_eq!(moved_from(&target).unwrap().as_deref(), Some(&*a.to_string_lossy()));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_locks_of_a_move_are_exclusive_and_create_nothing() {
        let root = temp_dir("locks");
        let path = root.join("control.lock");
        fs::write(&path, b"").unwrap();
        let held = OpenOptions::new().read(true).write(true).open(&path).unwrap();
        held.try_lock().unwrap();
        let err = lock_existing(&path, Some(Instant::now())).unwrap_err();
        assert!(err.to_string().contains("another process holds"), "{err}");
        drop(held);
        assert!(lock_existing(&path, Some(Instant::now())).unwrap().is_some());
        assert!(lock_existing(&root.join("none.lock"), None).unwrap().is_none());
        assert!(!root.join("none.lock").exists());
        let folder = lock_folder(&root, Some(Instant::now())).unwrap();
        assert!(lock_folder(&root, Some(Instant::now())).is_err());
        drop(folder);
        assert!(lock_folder(&root, Some(Instant::now())).is_ok());
        let _ = fs::remove_dir_all(&root);
    }
}
