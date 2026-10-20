//! Bounded, unprivileged adoption of pre-rename user directories (#505).
//!
//! Inspect every pair before changing anything. The only mutation is an atomic
//! relative sibling symlink; existing entries and stored data are never changed.
//! This guards session startup, not standalone shell tools or settings formats.

use std::ffi::OsString;
use std::fmt;
use std::path::{Component, PathBuf};

use rustix::fd::OwnedFd;
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags, Stat};
use rustix::io::Errno;

const DIRECTORY: OFlags = OFlags::PATH
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// A blocked upgrade leaves existing data intact, including conflicting copies.
#[derive(Debug)]
pub struct UpgradeError {
    old: PathBuf,
    new: PathBuf,
    reason: String,
}

impl fmt::Display for UpgradeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "settings upgrade blocked ({} -> {}): {}. No existing data was overwritten, merged, or deleted; any safe links already created are retained. Before retrying, use another login or TTY to back up and inspect both paths, then choose the intended settings location. See docs/legacy-upgrade.md",
            self.old.display(), self.new.display(), self.reason
        )
    }
}

impl std::error::Error for UpgradeError {}

type Result<T> = std::result::Result<T, UpgradeError>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Pair {
    base: PathBuf,
    old: &'static str,
    new: &'static str,
}

impl Pair {
    fn error(&self, reason: impl fmt::Display) -> UpgradeError {
        UpgradeError {
            old: self.base.join(self.old),
            new: self.base.join(self.new),
            reason: reason.to_string(),
        }
    }
}

#[derive(Clone, Copy)]
struct Identity {
    uid: u32,
    privileged: bool,
}

impl Identity {
    fn current() -> Self {
        let uid = rustix::process::getuid();
        Self {
            uid: uid.as_raw(),
            privileged: uid.as_raw() == 0
                || uid != rustix::process::geteuid()
                || rustix::process::getgid() != rustix::process::getegid()
                // SAFETY: getauxval reads this process's immutable auxiliary
                // vector; AT_SECURE also covers secure file-capability starts.
                || unsafe { libc::getauxval(libc::AT_SECURE) != 0 },
        }
    }
}

fn absolute(value: Option<OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|path| path.is_absolute())
}

fn pairs_from(env: impl Fn(&str) -> Option<OsString>) -> Vec<Pair> {
    // Match the actual readers, including their different relative-path rules.
    // Never redirect an unsupported relative cache/config migration elsewhere.
    let home = env("HOME").map(PathBuf::from);
    let config =
        absolute(env("XDG_CONFIG_HOME")).or_else(|| home.as_ref().map(|home| home.join(".config")));
    let cache = env("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".cache")));
    let data = absolute(env("XDG_DATA_HOME")).or_else(|| {
        home.as_ref()
            .filter(|home| home.is_absolute())
            .map(|home| home.join(".local/share"))
    });
    let state = absolute(env("XDG_STATE_HOME")).or_else(|| {
        home.as_ref()
            .filter(|home| home.is_absolute())
            .map(|home| home.join(".local/state"))
    });
    let mut pairs = Vec::new();
    // tuna-rename: keep-begin
    for (base, old, new) in [
        (config, "roost", "tuna"),
        (cache, "roost", "tuna"),
        (data.clone(), "roost", "tuna"),
        (data, "roost-shell", "tuna-shell"),
        (state, "roost-shell", "tuna-shell"),
    ] {
        // tuna-rename: keep-end
        if let Some(base) = base {
            let pair = Pair { base, old, new };
            if !pairs.contains(&pair) {
                pairs.push(pair);
            }
        }
    }
    pairs
}

fn optional(stat: std::result::Result<Stat, Errno>, pair: &Pair) -> Result<Option<Stat>> {
    match stat {
        Ok(stat) => Ok(Some(stat)),
        Err(Errno::NOENT) => Ok(None),
        Err(error) => Err(pair.error(error)),
    }
}

fn same_inode(left: &Stat, right: &Stat) -> bool {
    left.st_dev == right.st_dev && left.st_ino == right.st_ino
}

fn owned_directory(stat: &Stat, uid: u32) -> bool {
    FileType::from_raw_mode(stat.st_mode) == FileType::Directory
        && stat.st_uid == uid
        && stat.st_mode & 0o022 == 0
}

struct Node {
    name: OsString,
    fd: OwnedFd,
    original: Stat,
}

struct Anchor {
    // Keep every descriptor, not just the leaf: an ancestor rename must be
    // noticed before the link is created through a detached directory.
    nodes: Vec<Node>,
}

impl Anchor {
    fn open(pair: &Pair, uid: u32) -> Result<Self> {
        if !pair.base.is_absolute() {
            return Err(pair.error("automatic adoption requires an absolute base directory"));
        }
        let root = fs::open("/", DIRECTORY, Mode::empty()).map_err(|e| pair.error(e))?;
        let original = fs::fstat(&root).map_err(|e| pair.error(e))?;
        let mut anchor = Self {
            nodes: vec![Node {
                name: OsString::from("/"),
                fd: root,
                original,
            }],
        };
        for part in pair.base.components() {
            match part {
                Component::RootDir => continue,
                Component::Normal(name) => {
                    let fd =
                        fs::openat(anchor.base(), name, DIRECTORY, Mode::empty()).map_err(|e| {
                            pair.error(format!("cannot anchor base without symlinks: {e}"))
                        })?;
                    let original = fs::fstat(&fd).map_err(|e| pair.error(e))?;
                    anchor.nodes.push(Node {
                        name: name.to_owned(),
                        fd,
                        original,
                    });
                }
                _ => return Err(pair.error("automatic adoption does not accept parent traversal")),
            }
        }
        anchor.validate(pair, uid)?;
        Ok(anchor)
    }

    fn base(&self) -> &OwnedFd {
        &self.nodes.last().expect("anchor always contains root").fd
    }

    fn validate(&self, pair: &Pair, uid: u32) -> Result<()> {
        for (index, node) in self.nodes.iter().enumerate() {
            let stat = fs::fstat(&node.fd).map_err(|e| pair.error(e))?;
            let final_node = index + 1 == self.nodes.len();
            let trusted_owner = stat.st_uid == 0 || stat.st_uid == uid;
            // Sticky directories protect trusted-owned child entries from
            // other writers. They do not make an untrusted owner safe, or
            // protect arbitrary settings contents inside the final base.
            let safe_ancestor =
                trusted_owner && (stat.st_mode & 0o022 == 0 || stat.st_mode & 0o1000 != 0);
            if !same_inode(&stat, &node.original)
                || FileType::from_raw_mode(stat.st_mode) != FileType::Directory
                || !safe_ancestor
                || (final_node && !owned_directory(&stat, uid))
            {
                return Err(
                    pair.error("base ownership or ancestor replacement permissions are unsafe")
                );
            }
            if index != 0 {
                let bound = fs::statat(
                    &self.nodes[index - 1].fd,
                    node.name.as_os_str(),
                    AtFlags::SYMLINK_NOFOLLOW,
                )
                .map_err(|e| pair.error(e))?;
                if !same_inode(&stat, &bound)
                    || FileType::from_raw_mode(bound.st_mode) != FileType::Directory
                {
                    return Err(pair.error("base directory changed during inspection"));
                }
            }
        }
        Ok(())
    }
}

struct Adoption {
    pair: Pair,
    anchor: Anchor,
    legacy_fd: OwnedFd,
    legacy: Stat,
}

impl Adoption {
    /// Check the exact prior-adoption shape, never merely a resolving symlink.
    fn already_linked(&self, uid: u32) -> Result<bool> {
        let pair = &self.pair;
        let new = optional(
            fs::statat(self.anchor.base(), pair.new, AtFlags::SYMLINK_NOFOLLOW),
            pair,
        )?;
        let Some(new) = new else { return Ok(false) };
        if FileType::from_raw_mode(new.st_mode) == FileType::Symlink && new.st_uid == uid {
            let target = fs::readlinkat(self.anchor.base(), pair.new, Vec::new())
                .map_err(|e| pair.error(e))?;
            if target.as_bytes() == pair.old.as_bytes() {
                return Ok(true);
            }
        }
        Err(pair.error(
            "both paths exist, or the current path is an unexpected link; choose a copy explicitly",
        ))
    }

    fn validate(&self, uid: u32) -> Result<()> {
        self.anchor.validate(&self.pair, uid)?;
        let pinned = fs::fstat(&self.legacy_fd).map_err(|e| self.pair.error(e))?;
        let old = fs::statat(self.anchor.base(), self.pair.old, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|e| self.pair.error(e))?;
        if !owned_directory(&pinned, uid)
            || !owned_directory(&old, uid)
            || !same_inode(&old, &self.legacy)
            || !same_inode(&old, &pinned)
        {
            return Err(self
                .pair
                .error("legacy directory ownership, type, permissions or identity changed"));
        }
        Ok(())
    }
}

fn inspect(pairs: Vec<Pair>, identity: Identity) -> Result<Vec<Adoption>> {
    let mut ready = Vec::new();
    for pair in pairs {
        let old = optional(fs::lstat(pair.base.join(pair.old)), &pair)?;
        if old.is_none() {
            // No migration: retain relative current-only paths. Leaf links,
            // wrong owners/types and writable shared settings still fail
            // explicitly rather than letting a dangling link look like absence.
            if let Some(new) = optional(fs::lstat(pair.base.join(pair.new)), &pair)? {
                if !owned_directory(&new, identity.uid) {
                    return Err(pair.error(
                        "current-only path must be an owned real directory without other writers",
                    ));
                }
            }
            continue;
        }
        if identity.privileged {
            return Err(
                pair.error("automatic adoption is unavailable to privileged or set-id processes")
            );
        }
        let anchor = Anchor::open(&pair, identity.uid)?;
        let legacy_fd = fs::openat(anchor.base(), pair.old, DIRECTORY, Mode::empty())
            .map_err(|e| pair.error(e))?;
        let legacy = fs::fstat(&legacy_fd).map_err(|e| pair.error(e))?;
        if !owned_directory(&legacy, identity.uid) {
            return Err(
                pair.error("legacy path must be an owned real directory without other writers")
            );
        }
        let adoption = Adoption {
            pair,
            anchor,
            legacy_fd,
            legacy,
        };
        adoption.already_linked(identity.uid)?;
        ready.push(adoption);
    }
    Ok(ready)
}

fn apply(ready: &[Adoption], uid: u32) -> Result<()> {
    // Revalidate all inspected candidates before the first mutation, then each
    // candidate again immediately before creation. No rollback ever deletes an
    // entry; a late failure leaves only completed links for a safe retry.
    for adoption in ready {
        adoption.validate(uid)?;
        adoption.already_linked(uid)?;
    }
    for adoption in ready {
        adoption.validate(uid)?;
        if adoption.already_linked(uid)? {
            continue;
        }
        let pair = &adoption.pair;
        // symlinkat is no-replace, including dangling destinations. If another
        // starter wins the race, accept only its exact safe adoption shape.
        match fs::symlinkat(pair.old, adoption.anchor.base(), pair.new) {
            Ok(()) => {}
            Err(Errno::EXIST) if adoption.already_linked(uid)? => {}
            Err(error) => return Err(pair.error(error)),
        }
        adoption.validate(uid)?;
        if !adoption.already_linked(uid)? {
            return Err(pair.error("adoption link changed during creation"));
        }
    }
    Ok(())
}

/// Prepare the current user's settings before compositor/session writers start.
/// Help/version and the internal PAM helper must exit before calling this.
pub fn prepare() -> Result<()> {
    let identity = Identity::current();
    let ready = inspect(pairs_from(|key| std::env::var_os(key)), identity)?;
    apply(&ready, identity.uid)
}

#[cfg(test)]
#[path = "user_upgrade_tests.rs"]
mod tests;
