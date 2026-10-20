//! Real-filesystem checks for the production startup adoption guard.
//!
//! These tests do not run a desktop, native settings readers, or PAM. Identity
//! rejection tests model privileged/foreign identities; they never chown, set-id,
//! change process environment/cwd, or touch a live user's settings.
// tuna-rename: keep-begin

use super::*;
use std::collections::BTreeMap;
use std::fs as stdfs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::Path;

fn identity() -> Identity {
    let identity = Identity::current();
    assert!(
        !identity.privileged,
        "filesystem adoption tests must run as an ordinary, non-set-id user"
    );
    identity
}

fn mkdir(path: &Path) {
    stdfs::create_dir_all(path).unwrap();
    chmod(path, 0o700);
}

fn chmod(path: &Path, mode: u32) {
    stdfs::set_permissions(path, stdfs::Permissions::from_mode(mode)).unwrap();
}

fn resolved(values: &[(&str, &Path)]) -> Vec<Pair> {
    pairs_from(|key| {
        values
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| value.as_os_str().to_owned())
    })
}

struct Fixture {
    root: tempfile::TempDir,
    pairs: Vec<Pair>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        mkdir(&home);
        let pairs = resolved(&[("HOME", &home)]);
        assert_eq!(pairs.len(), 5);
        for pair in &pairs {
            mkdir(&pair.base);
        }
        Self { root, pairs }
    }

    fn seed_old(&self) {
        for (index, pair) in self.pairs.iter().enumerate() {
            seed(&pair.base.join(pair.old), index);
        }
    }

    fn run(&self) -> Result<()> {
        let user = identity();
        apply(&inspect(self.pairs.clone(), user)?, user.uid)
    }

    fn snapshot(&self) -> Snapshot {
        snapshot(self.root.path())
    }
}

fn seed(directory: &Path, index: usize) {
    mkdir(directory);
    mkdir(&directory.join("nested"));
    for (name, bytes) in [
        ("monitor-layout.json", b"{\"scale\":1.5}\n".as_slice()),
        ("favorites.json", b"[\"org.example.Editor\"]\n".as_slice()),
        ("notification-history.json", b"[1,2,3]\n".as_slice()),
        ("nested/binary", &[0, 255, index as u8]),
    ] {
        stdfs::write(directory.join(name), bytes).unwrap();
    }
    // Existing links inside settings are data, not candidates for rewriting.
    symlink("nested/binary", directory.join("existing-link")).unwrap();
}

#[derive(Debug, Eq, PartialEq)]
enum Contents {
    Directory,
    File(Vec<u8>),
    Link(PathBuf),
}

#[derive(Debug, Eq, PartialEq)]
struct Entry {
    device: u64,
    inode: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    contents: Contents,
}

type Snapshot = BTreeMap<PathBuf, Entry>;

fn snapshot(root: &Path) -> Snapshot {
    fn visit(root: &Path, path: &Path, entries: &mut Snapshot) {
        let metadata = stdfs::symlink_metadata(path).unwrap();
        let contents = if metadata.is_symlink() {
            Contents::Link(stdfs::read_link(path).unwrap())
        } else if metadata.is_dir() {
            Contents::Directory
        } else {
            assert!(metadata.is_file(), "unexpected fixture type: {path:?}");
            Contents::File(stdfs::read(path).unwrap())
        };
        entries.insert(
            path.strip_prefix(root).unwrap().to_owned(),
            Entry {
                device: metadata.dev(),
                inode: metadata.ino(),
                uid: metadata.uid(),
                gid: metadata.gid(),
                mode: metadata.mode(),
                contents,
            },
        );
        if metadata.is_dir() {
            for entry in stdfs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), entries);
            }
        }
    }
    // Reading may update atime; creating a link changes its parent's timestamps
    // and directory size. Neither is part of the stored-data preservation claim.
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

fn assert_preserved(before: &Snapshot, after: &Snapshot) {
    for (name, entry) in before {
        assert_eq!(after.get(name), Some(entry), "pre-existing entry {name:?}");
    }
}

fn assert_blocked_unchanged(fixture: &Fixture, expected: &str) -> String {
    let before = fixture.snapshot();
    let error = fixture.run().unwrap_err().to_string();
    assert!(error.contains(expected), "{error}");
    assert_eq!(before, fixture.snapshot());
    error
}

fn assert_links(fixture: &Fixture) {
    for pair in &fixture.pairs {
        assert_eq!(
            stdfs::read_link(pair.base.join(pair.new)).unwrap(),
            Path::new(pair.old)
        );
        for file in [
            "monitor-layout.json",
            "favorites.json",
            "notification-history.json",
            "nested/binary",
            "existing-link",
        ] {
            assert_eq!(
                stdfs::read(pair.base.join(pair.old).join(file)).unwrap(),
                stdfs::read(pair.base.join(pair.new).join(file)).unwrap(),
                "both reader paths must retain the same bytes: {pair:?}/{file}"
            );
        }
    }
}

#[test]
fn all_five_pairs_preserve_bytes_entries_and_old_reader_paths() {
    let fixture = Fixture::new();
    fixture.seed_old();
    let before = fixture.snapshot();
    fixture.run().unwrap();
    assert_links(&fixture);
    let after = fixture.snapshot();
    assert_preserved(&before, &after);
    assert_eq!(after.len(), before.len() + 5, "only five links may appear");
    for _ in 0..3 {
        fixture.run().unwrap();
        assert_eq!(after, fixture.snapshot());
    }
}

#[test]
fn fresh_and_current_only_states_are_unchanged_for_every_pair() {
    let fixture = Fixture::new();
    let before = fixture.snapshot();
    fixture.run().unwrap();
    assert_eq!(before, fixture.snapshot());
    for (index, pair) in fixture.pairs.iter().enumerate() {
        seed(&pair.base.join(pair.new), index);
    }
    let before = fixture.snapshot();
    fixture.run().unwrap();
    assert_eq!(before, fixture.snapshot());
}

#[test]
fn missing_bases_are_not_created() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("missing-home");
    let before = snapshot(root.path());
    let user = identity();
    let ready = inspect(resolved(&[("HOME", &home)]), user).unwrap();
    assert!(ready.is_empty());
    apply(&ready, user.uid).unwrap();
    assert_eq!(before, snapshot(root.path()));
}

#[test]
fn a_conflict_in_any_pair_is_found_before_any_link_is_created() {
    for conflict in 0..5 {
        let fixture = Fixture::new();
        fixture.seed_old();
        let pair = &fixture.pairs[conflict];
        seed(&pair.base.join(pair.new), 99);
        let error = assert_blocked_unchanged(&fixture, "both paths exist");
        assert!(error.contains(&pair.base.join(pair.old).display().to_string()));
        assert!(error.contains(&pair.base.join(pair.new).display().to_string()));
        for other in fixture.pairs.iter().filter(|other| *other != pair) {
            assert!(stdfs::symlink_metadata(other.base.join(other.new)).is_err());
        }
    }
}

#[test]
fn recovery_error_names_both_paths_and_preservation_limits() {
    let fixture = Fixture::new();
    fixture.seed_old();
    mkdir(&fixture.pairs[0].base.join(fixture.pairs[0].new));
    let error = assert_blocked_unchanged(&fixture, "both paths exist");
    for guidance in [
        "No existing data was overwritten, merged, or deleted",
        "safe links already created are retained",
        "back up and inspect both paths",
        "choose the intended settings location",
        "docs/legacy-upgrade.md",
    ] {
        assert!(error.contains(guidance), "missing guidance: {guidance}");
    }
}

#[test]
fn old_regular_files_and_links_are_rejected_in_every_pair() {
    for index in 0..5 {
        for kind in ["file", "directory-link", "dangling-link"] {
            let fixture = Fixture::new();
            let pair = &fixture.pairs[index];
            let old = pair.base.join(pair.old);
            match kind {
                "file" => stdfs::write(&old, b"original").unwrap(),
                "directory-link" => {
                    mkdir(&pair.base.join("elsewhere"));
                    symlink("elsewhere", &old).unwrap();
                }
                "dangling-link" => symlink("missing", &old).unwrap(),
                _ => unreachable!(),
            }
            // O_PATH|DIRECTORY|NOFOLLOW can reject before metadata inspection.
            assert_blocked_unchanged(&fixture, "settings upgrade blocked");
        }
    }
}

#[test]
fn current_only_files_and_links_are_rejected_in_every_pair() {
    for index in 0..5 {
        for kind in ["file", "directory-link", "dangling-link", "old-sibling"] {
            let fixture = Fixture::new();
            let pair = &fixture.pairs[index];
            let new = pair.base.join(pair.new);
            match kind {
                "file" => stdfs::write(&new, b"original").unwrap(),
                "directory-link" => {
                    mkdir(&pair.base.join("elsewhere"));
                    symlink("elsewhere", &new).unwrap();
                }
                "dangling-link" => symlink("missing", &new).unwrap(),
                "old-sibling" => symlink(pair.old, &new).unwrap(),
                _ => unreachable!(),
            }
            assert_blocked_unchanged(
                &fixture,
                "current-only path must be an owned real directory",
            );
        }
    }
}

#[test]
fn only_the_exact_relative_old_sibling_link_is_accepted() {
    for index in 0..5 {
        for kind in [
            "absolute",
            "dot-relative",
            "parent-relative",
            "other",
            "dangling",
        ] {
            let fixture = Fixture::new();
            fixture.seed_old();
            let pair = &fixture.pairs[index];
            let target = match kind {
                "absolute" => pair.base.join(pair.old),
                "dot-relative" => PathBuf::from(format!("./{}", pair.old)),
                "parent-relative" => Path::new("..")
                    .join(pair.base.file_name().unwrap())
                    .join(pair.old),
                "other" => {
                    mkdir(&pair.base.join("elsewhere"));
                    PathBuf::from("elsewhere")
                }
                "dangling" => PathBuf::from("missing"),
                _ => unreachable!(),
            };
            symlink(target, pair.base.join(pair.new)).unwrap();
            assert_blocked_unchanged(&fixture, "unexpected link");
        }
    }
}

#[test]
fn an_exact_prior_link_does_not_bypass_legacy_directory_safety() {
    let fixture = Fixture::new();
    let pair = &fixture.pairs[0];
    mkdir(&pair.base.join("elsewhere"));
    symlink("elsewhere", pair.base.join(pair.old)).unwrap();
    symlink(pair.old, pair.base.join(pair.new)).unwrap();
    assert_blocked_unchanged(&fixture, "settings upgrade blocked");
}

#[test]
fn writable_old_and_current_directories_are_rejected_even_with_sticky_bit() {
    for old in [true, false] {
        for mode in [0o720, 0o702, 0o777, 0o1720, 0o1702, 0o1777] {
            let fixture = Fixture::new();
            let pair = &fixture.pairs[0];
            let directory = pair.base.join(if old { pair.old } else { pair.new });
            mkdir(&directory);
            chmod(&directory, mode);
            assert_blocked_unchanged(&fixture, "without other writers");
        }
    }
}

#[test]
fn writable_final_bases_are_rejected_even_with_sticky_bit() {
    for mode in [0o720, 0o702, 0o777, 0o1720, 0o1702, 0o1777] {
        let fixture = Fixture::new();
        fixture.seed_old();
        chmod(&fixture.pairs[0].base, mode);
        assert_blocked_unchanged(&fixture, "ancestor replacement permissions are unsafe");
    }
}

#[test]
fn foreign_uid_and_privileged_identities_are_modeled_without_chown_or_setid() {
    let fixture = Fixture::new();
    fixture.seed_old();
    let before = fixture.snapshot();
    let user = identity();
    let foreign = Identity {
        uid: user.uid + 1,
        privileged: false,
    };
    assert!(inspect(fixture.pairs.clone(), foreign).is_err());
    for uid in [0, user.uid] {
        let error = inspect(
            fixture.pairs.clone(),
            Identity {
                uid,
                privileged: true,
            },
        )
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("privileged or set-id processes"));
    }
    assert_eq!(before, fixture.snapshot());
}

#[test]
fn current_only_foreign_uid_is_rejected() {
    let fixture = Fixture::new();
    mkdir(&fixture.pairs[0].base.join(fixture.pairs[0].new));
    let before = fixture.snapshot();
    let error = inspect(
        fixture.pairs.clone(),
        Identity {
            uid: identity().uid + 1,
            privileged: false,
        },
    )
    .err()
    .unwrap()
    .to_string();
    assert!(error.contains("current-only path must be an owned real directory"));
    assert_eq!(before, fixture.snapshot());
}

#[test]
fn modeled_foreign_owned_legacy_metadata_is_not_an_owned_directory() {
    let fixture = Fixture::new();
    fixture.seed_old();
    let pair = &fixture.pairs[0];
    let mut stat = fs::lstat(pair.base.join(pair.old)).unwrap();
    let uid = identity().uid;
    assert!(owned_directory(&stat, uid));
    stat.st_uid = uid + 1;
    assert!(!owned_directory(&stat, uid));
    stat.st_uid = 0;
    assert!(!owned_directory(&stat, uid));
}

#[test]
fn prior_link_with_a_modeled_foreign_uid_is_rejected() {
    let fixture = Fixture::new();
    fixture.seed_old();
    let user = identity();
    let ready = inspect(fixture.pairs.clone(), user).unwrap();
    let pair = &fixture.pairs[0];
    symlink(pair.old, pair.base.join(pair.new)).unwrap();
    let before = fixture.snapshot();
    assert!(ready[0].already_linked(user.uid).unwrap());
    assert!(ready[0].already_linked(user.uid + 1).is_err());
    assert_eq!(before, fixture.snapshot());
}

#[test]
fn a_removed_legacy_directory_is_detected_before_any_mutation() {
    let fixture = Fixture::new();
    fixture.seed_old();
    let user = identity();
    let ready = inspect(fixture.pairs.clone(), user).unwrap();
    let pair = &fixture.pairs[4];
    stdfs::rename(pair.base.join(pair.old), pair.base.join("removed-old")).unwrap();
    let before = fixture.snapshot();
    assert!(apply(&ready, user.uid).is_err());
    assert_eq!(before, fixture.snapshot());
}

#[test]
fn a_deleted_then_recreated_legacy_directory_cannot_reuse_the_pinned_identity() {
    let fixture = Fixture::new();
    let pair = &fixture.pairs[0];
    let old = pair.base.join(pair.old);
    mkdir(&old);
    let user = identity();
    let ready = inspect(vec![pair.clone()], user).unwrap();
    let pinned = fs::fstat(&ready[0].legacy_fd).unwrap();
    // Delete only the empty disposable fixture to exercise inode pinning, rather
    // than retaining the old inode through a renamed directory entry.
    stdfs::remove_dir(&old).unwrap();
    mkdir(&old);
    let replacement = fs::lstat(&old).unwrap();
    assert!(!same_inode(&pinned, &replacement));
    let before = fixture.snapshot();
    assert!(apply(&ready, user.uid).is_err());
    assert_eq!(before, fixture.snapshot());
}

#[test]
fn postrename_state_directory_is_not_a_sixth_migration_pair() {
    let fixture = Fixture::new();
    fixture.seed_old();
    let state = &fixture.pairs[4].base;
    seed(&state.join("tuna"), 91);
    seed(&state.join("roost"), 92);
    let before = fixture.snapshot();
    fixture.run().unwrap();
    let after = fixture.snapshot();
    assert_preserved(&before, &after);
    assert_eq!(after.len(), before.len() + 5);
    assert!(stdfs::symlink_metadata(state.join("tuna"))
        .unwrap()
        .is_dir());
    assert_links(&fixture);
}

#[test]
fn writable_nonsticky_ancestors_are_rejected_but_trusted_sticky_ancestors_work() {
    for mode in [0o777, 0o1777] {
        let fixture = Fixture::new();
        fixture.seed_old();
        chmod(fixture.root.path(), mode);
        if mode == 0o777 {
            assert_blocked_unchanged(&fixture, "ancestor replacement permissions are unsafe");
        } else {
            let before = fixture.snapshot();
            fixture.run().unwrap();
            assert_preserved(&before, &fixture.snapshot());
            assert_links(&fixture);
        }
    }
}

#[test]
fn symlinked_base_or_ancestor_is_rejected_without_following_it_for_mutation() {
    for ancestor in [false, true] {
        let fixture = Fixture::new();
        fixture.seed_old();
        let original = if ancestor {
            fixture.root.path().join("home")
        } else {
            fixture.pairs[0].base.clone()
        };
        let moved = original.with_extension("moved");
        stdfs::rename(&original, &moved).unwrap();
        symlink(&moved, &original).unwrap();
        assert_blocked_unchanged(&fixture, "cannot anchor base without symlinks");
    }
}

#[test]
fn parent_traversal_is_not_an_adoption_base() {
    let fixture = Fixture::new();
    fixture.seed_old();
    let mut pair = fixture.pairs[0].clone();
    pair.base = pair.base.join("..").join(".config");
    let before = fixture.snapshot();
    let error = inspect(vec![pair], identity()).err().unwrap().to_string();
    assert!(error.contains("parent traversal"));
    assert_eq!(before, fixture.snapshot());
}

#[test]
fn permission_denied_inspection_does_not_create_earlier_links() {
    let fixture = Fixture::new();
    fixture.seed_old();
    let blocked = &fixture.pairs[4].base;
    let before = fixture.snapshot();
    chmod(blocked, 0o600);
    let error = fixture.run().unwrap_err().to_string();
    chmod(blocked, 0o700);
    assert!(
        error.contains("Permission denied") || error.contains("permission denied"),
        "{error}"
    );
    assert_eq!(before, fixture.snapshot());
}

#[test]
fn resolver_uses_exact_five_pairs_and_absolute_xdg_overrides() {
    let root = Path::new("/disposable-test-values");
    let pairs = resolved(&[
        ("HOME", Path::new("/ignored-home")),
        ("XDG_CONFIG_HOME", &root.join("config")),
        ("XDG_CACHE_HOME", &root.join("cache")),
        ("XDG_DATA_HOME", &root.join("data")),
        ("XDG_STATE_HOME", &root.join("state")),
    ]);
    let actual: Vec<_> = pairs
        .iter()
        .map(|pair| (pair.base.clone(), pair.old, pair.new))
        .collect();
    assert_eq!(
        actual,
        vec![
            (root.join("config"), "roost", "tuna"),
            (root.join("cache"), "roost", "tuna"),
            (root.join("data"), "roost", "tuna"),
            (root.join("data"), "roost-shell", "tuna-shell"),
            (root.join("state"), "roost-shell", "tuna-shell"),
        ]
    );
}

#[test]
fn resolver_matches_different_relative_xdg_reader_semantics() {
    let pairs = resolved(&[
        ("HOME", Path::new("/home-fixture")),
        ("XDG_CONFIG_HOME", Path::new("relative-config")),
        ("XDG_CACHE_HOME", Path::new("relative-cache")),
        ("XDG_DATA_HOME", Path::new("relative-data")),
        ("XDG_STATE_HOME", Path::new("relative-state")),
    ]);
    assert_eq!(pairs[0].base, Path::new("/home-fixture/.config"));
    assert_eq!(pairs[1].base, Path::new("relative-cache"));
    assert_eq!(pairs[2].base, Path::new("/home-fixture/.local/share"));
    assert_eq!(pairs[3].base, Path::new("/home-fixture/.local/share"));
    assert_eq!(pairs[4].base, Path::new("/home-fixture/.local/state"));
}

#[test]
fn relative_and_empty_home_only_resolve_config_and_cache() {
    for home in [Path::new("relative-home"), Path::new("")] {
        let pairs = resolved(&[("HOME", home)]);
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].base, home.join(".config"));
        assert_eq!(pairs[1].base, home.join(".cache"));
    }
}

#[test]
fn empty_cache_override_stays_empty_instead_of_falling_back_to_home() {
    let pairs = resolved(&[
        ("HOME", Path::new("/home-fixture")),
        ("XDG_CACHE_HOME", Path::new("")),
    ]);
    assert_eq!(pairs[1].base, Path::new(""));
}

#[test]
fn missing_home_never_invents_a_fallback_root() {
    assert!(resolved(&[]).is_empty());
    let pairs = resolved(&[
        ("XDG_CONFIG_HOME", Path::new("relative-config")),
        ("XDG_CACHE_HOME", Path::new("relative-cache")),
        ("XDG_DATA_HOME", Path::new("relative-data")),
        ("XDG_STATE_HOME", Path::new("relative-state")),
    ]);
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0].base, Path::new("relative-cache"));
    let pairs = resolved(&[("XDG_DATA_HOME", Path::new("/absolute-data"))]);
    assert_eq!(pairs.len(), 2);
    assert!(pairs
        .iter()
        .all(|pair| pair.base == Path::new("/absolute-data")));
}

#[test]
fn overlapping_xdg_bases_deduplicate_identical_pairs() {
    let root = Path::new("/same-fixture-base");
    let pairs = resolved(&[
        ("XDG_CONFIG_HOME", root),
        ("XDG_CACHE_HOME", root),
        ("XDG_DATA_HOME", root),
        ("XDG_STATE_HOME", root),
    ]);
    assert_eq!(pairs.len(), 2);
    assert_eq!((pairs[0].old, pairs[0].new), ("roost", "tuna"));
    assert_eq!((pairs[1].old, pairs[1].new), ("roost-shell", "tuna-shell"));
}

#[test]
fn absolute_xdg_roots_work_without_home() {
    let fixture = Fixture::new();
    fixture.seed_old();
    let pairs = resolved(&[
        ("XDG_CONFIG_HOME", &fixture.pairs[0].base),
        ("XDG_CACHE_HOME", &fixture.pairs[1].base),
        ("XDG_DATA_HOME", &fixture.pairs[2].base),
        ("XDG_STATE_HOME", &fixture.pairs[4].base),
    ]);
    let user = identity();
    apply(&inspect(pairs, user).unwrap(), user.uid).unwrap();
    assert_links(&fixture);
}

#[test]
fn relative_config_and_cache_preserve_nonadoption_but_block_old_only() {
    // A disposable child of the existing cwd gives genuinely relative paths
    // without process-global set_current_dir or environment changes.
    let cwd = std::env::current_dir().unwrap();
    let root = tempfile::Builder::new()
        .prefix(".adoption-relative-")
        .tempdir_in(&cwd)
        .unwrap();
    let relative = root.path().strip_prefix(&cwd).unwrap();
    let home = relative.join("home");
    let cache = relative.join("cache");
    let pairs = resolved(&[("HOME", &home), ("XDG_CACHE_HOME", &cache)]);
    assert_eq!(pairs.len(), 2);
    let user = identity();
    apply(&inspect(pairs.clone(), user).unwrap(), user.uid).unwrap();
    for pair in &pairs {
        mkdir(&pair.base);
        seed(&pair.base.join(pair.new), 1);
    }
    let before = snapshot(root.path());
    apply(&inspect(pairs.clone(), user).unwrap(), user.uid).unwrap();
    assert_eq!(before, snapshot(root.path()));
    for pair in &pairs {
        // Move only this disposable fixture's current directory to its old name.
        stdfs::rename(pair.base.join(pair.new), pair.base.join(pair.old)).unwrap();
        let before = snapshot(root.path());
        let error = inspect(vec![pair.clone()], user).err().unwrap().to_string();
        assert!(error.contains("absolute base directory"), "{error}");
        assert_eq!(before, snapshot(root.path()));
    }
}

#[test]
fn relative_cache_migration_does_not_redirect_to_home_cache() {
    let cwd = std::env::current_dir().unwrap();
    let root = tempfile::Builder::new()
        .prefix(".adoption-relative-")
        .tempdir_in(&cwd)
        .unwrap();
    let home = root.path().join("home");
    let cache = root.path().strip_prefix(&cwd).unwrap().join("cache");
    mkdir(&home.join(".cache/roost"));
    mkdir(&cache.join("roost"));
    let before = snapshot(root.path());
    let pairs = resolved(&[("HOME", &home), ("XDG_CACHE_HOME", &cache)]);
    let error = inspect(pairs, identity()).err().unwrap().to_string();
    assert!(error.contains("absolute base directory"));
    assert_eq!(before, snapshot(root.path()));
}

#[test]
fn ancestor_or_final_base_replacement_after_inspection_is_detected() {
    for ancestor in [true, false] {
        let fixture = Fixture::new();
        fixture.seed_old();
        let user = identity();
        let ready = inspect(fixture.pairs.clone(), user).unwrap();
        let original = if ancestor {
            fixture.root.path().join("home")
        } else {
            fixture.pairs[0].base.clone()
        };
        let moved = original.with_extension("moved");
        stdfs::rename(&original, &moved).unwrap();
        mkdir(&original);
        let before = fixture.snapshot();
        let error = apply(&ready, user.uid).unwrap_err().to_string();
        assert!(error.contains("base directory changed"), "{error}");
        assert_eq!(
            before,
            fixture.snapshot(),
            "neither replacement nor detached directory may be mutated"
        );
    }
}

#[test]
fn ancestor_permissions_are_revalidated_after_inspection() {
    let fixture = Fixture::new();
    fixture.seed_old();
    let user = identity();
    let ready = inspect(fixture.pairs.clone(), user).unwrap();
    chmod(&fixture.root.path().join("home"), 0o777);
    let before = fixture.snapshot();
    assert!(apply(&ready, user.uid).is_err());
    assert_eq!(before, fixture.snapshot());
}

#[test]
fn old_replacement_link_or_permission_change_after_inspection_is_detected() {
    for kind in ["replacement", "symlink", "writable"] {
        let fixture = Fixture::new();
        fixture.seed_old();
        let user = identity();
        let ready = inspect(fixture.pairs.clone(), user).unwrap();
        let pair = &fixture.pairs[4];
        let old = pair.base.join(pair.old);
        if kind == "writable" {
            chmod(&old, 0o777);
        } else {
            stdfs::rename(&old, pair.base.join("saved-old")).unwrap();
            if kind == "symlink" {
                symlink("saved-old", &old).unwrap();
            } else {
                mkdir(&old);
            }
        }
        let before = fixture.snapshot();
        let error = apply(&ready, user.uid).unwrap_err().to_string();
        assert!(
            error.contains("legacy directory ownership, type, permissions or identity changed"),
            "{error}"
        );
        assert_eq!(
            before,
            fixture.snapshot(),
            "all candidates must be revalidated before mutation"
        );
    }
}

#[test]
fn a_destination_created_after_inspection_is_never_replaced() {
    for kind in ["file", "directory", "dangling-link", "unexpected-link"] {
        let fixture = Fixture::new();
        fixture.seed_old();
        let user = identity();
        let ready = inspect(fixture.pairs.clone(), user).unwrap();
        let pair = &fixture.pairs[4];
        let destination = pair.base.join(pair.new);
        match kind {
            "file" => stdfs::write(&destination, b"concurrent writer").unwrap(),
            "directory" => seed(&destination, 77),
            "dangling-link" => symlink("missing", &destination).unwrap(),
            "unexpected-link" => symlink(format!("./{}", pair.old), &destination).unwrap(),
            _ => unreachable!(),
        }
        let before = fixture.snapshot();
        let error = apply(&ready, user.uid).unwrap_err().to_string();
        assert!(error.contains("both paths exist"));
        assert_eq!(before, fixture.snapshot());
    }
}

#[test]
fn an_exact_adoption_completed_by_another_starter_is_accepted() {
    let fixture = Fixture::new();
    fixture.seed_old();
    let user = identity();
    let first = inspect(fixture.pairs.clone(), user).unwrap();
    let second = inspect(fixture.pairs.clone(), user).unwrap();
    apply(&first, user.uid).unwrap();
    let before = fixture.snapshot();
    apply(&second, user.uid).unwrap();
    assert_eq!(before, fixture.snapshot());
    assert_links(&fixture);
}

#[test]
fn concurrent_starters_converge_without_overwriting_settings() {
    let fixture = Fixture::new();
    fixture.seed_old();
    let before = fixture.snapshot();
    let barrier = std::sync::Barrier::new(4);
    std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for _ in 0..4 {
            let pairs = fixture.pairs.clone();
            let barrier = &barrier;
            workers.push(scope.spawn(move || {
                let user = identity();
                let ready = inspect(pairs, user).unwrap();
                barrier.wait();
                apply(&ready, user.uid)
            }));
        }
        for worker in workers {
            worker.join().unwrap().unwrap();
        }
    });
    let after = fixture.snapshot();
    assert_preserved(&before, &after);
    assert_eq!(after.len(), before.len() + 5);
    assert_links(&fixture);
}

#[test]
fn late_creation_failure_preserves_completed_links_and_retry_is_safe() {
    let fixture = Fixture::new();
    fixture.seed_old();
    let user = identity();
    let blocked = &fixture.pairs[1].base;
    chmod(blocked, 0o500);
    let before = fixture.snapshot();
    // An owned read-only base passes inspection and preflight validation, then
    // symlinkat fails after the first pair was really applied in this same call.
    let ready = inspect(fixture.pairs.clone(), user).unwrap();
    let error = apply(&ready, user.uid).unwrap_err().to_string();
    assert!(
        error.contains("Permission denied") || error.contains("permission denied"),
        "{error}"
    );
    let after = fixture.snapshot();
    assert_preserved(&before, &after);
    assert_eq!(after.len(), before.len() + 1);
    assert_eq!(
        stdfs::read_link(fixture.pairs[0].base.join(fixture.pairs[0].new)).unwrap(),
        Path::new(fixture.pairs[0].old)
    );
    for pair in &fixture.pairs[1..] {
        assert!(stdfs::symlink_metadata(pair.base.join(pair.new)).is_err());
    }
    chmod(blocked, 0o700);
    fixture.run().unwrap();
    assert_links(&fixture);
}

#[test]
fn startup_source_exits_for_help_version_and_pam_before_guard_and_run() {
    // This is a source-order regression only. It does not claim that a native
    // compositor binary or its GTK/GLib/PAM dependencies built or executed.
    let source = include_str!("main.rs");
    let guard = source
        .find("tuna_compositor::user_upgrade::prepare()")
        .unwrap();
    let run = source.find("match run(&session)").unwrap();
    assert!(guard < run);
    let pam = source
        .find("return tuna_compositor::pam_helper::entry();")
        .unwrap();
    assert!(pam < guard);
    for arm in ["\"--version\" | \"-V\" =>", "\"--help\" | \"-h\" =>"] {
        let begin = source.find(arm).unwrap();
        let exit = source[begin..].find("return ExitCode::SUCCESS;").unwrap() + begin;
        assert!(begin < exit && exit < guard);
    }
    assert!(source[guard..run].contains("return ExitCode::FAILURE;"));
}

// tuna-rename: keep-end
