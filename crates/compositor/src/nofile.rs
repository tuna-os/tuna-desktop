//! The open-file limit (`RLIMIT_NOFILE`).
//!
//! Every client connection, shared-memory pool and sync file is a
//! descriptor, and systemd starts user sessions at a soft limit of 1024: a
//! burst of clients would otherwise exhaust it. Like Mutter, the compositor
//! raises its soft limit to the hard one at startup
//! (`meta_context_raise_rlimit_nofile` in mutter/src/core/meta-context.c,
//! called from mutter.c and gnome-shell's main.c; niri's
//! `store_and_increase_nofile_rlimit` does the same) and gives the processes
//! it starts the original limit back (`meta_context_restore_rlimit_nofile`
//! in Mutter's and GNOME Shell's child setup): programs that use `select()`
//! break past 1024 descriptors.

use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::OnceLock;

/// The limit this process started with, once [`raise`] ran.
static ORIGINAL: OnceLock<libc::rlimit> = OnceLock::new();

/// `limit` with its soft value at the hard one.
fn raised(limit: libc::rlimit) -> libc::rlimit {
    libc::rlimit {
        rlim_cur: limit.rlim_max,
        rlim_max: limit.rlim_max,
    }
}

/// Remember the starting limit and raise the soft limit to the hard one.
/// Returns the (old, new) soft limits, or `None` when the limit could not
/// be read or changed (the session runs on with the old one).
pub fn raise() -> Option<(u64, u64)> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes one rlimit through a valid pointer.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return None;
    }
    let _ = ORIGINAL.set(limit);
    let new = raised(limit);
    // SAFETY: a valid resource and an initialized rlimit for this process.
    if new.rlim_cur != limit.rlim_cur && unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &new) } != 0
    {
        return None;
    }
    Some((limit.rlim_cur, new.rlim_cur))
}

/// Start `command` with the open-file limit this process started with.
pub fn restore_in_child(command: &mut Command) -> &mut Command {
    let Some(&original) = ORIGINAL.get() else {
        return command;
    };
    // SAFETY: setrlimit is async-signal-safe; the closure only reads a copy.
    unsafe {
        command.pre_exec(move || {
            libc::setrlimit(libc::RLIMIT_NOFILE, &original);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raising_takes_the_hard_limit_and_keeps_it() {
        let raised = raised(libc::rlimit {
            rlim_cur: 1024,
            rlim_max: 524_288,
        });
        assert_eq!((raised.rlim_cur, raised.rlim_max), (524_288, 524_288));
    }

    #[test]
    fn children_start_with_the_original_limit() {
        let (old, new) = raise().expect("RLIMIT_NOFILE is readable and raisable");
        let mut current = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit writes one rlimit through a valid pointer.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut current) },
            0
        );
        assert_eq!(current.rlim_cur, new);
        assert_eq!(current.rlim_cur, current.rlim_max);
        let output = restore_in_child(Command::new("/bin/sh").args(["-c", "ulimit -Sn"]))
            .output()
            .expect("sh runs");
        let child: String = String::from_utf8_lossy(&output.stdout).trim().into();
        let expected = if old == libc::RLIM_INFINITY {
            "unlimited".to_owned()
        } else {
            old.to_string()
        };
        assert_eq!(child, expected);
    }
}
