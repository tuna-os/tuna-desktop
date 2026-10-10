//! Linux readiness for owned, unreaped children; no PID polling or /proc scan.
use calloop::generic::Generic;
use std::os::fd::OwnedFd;

pub(crate) fn source(pid: u32) -> std::io::Result<Generic<OwnedFd>> {
    let pid = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid owned child pid")
        })?;
    let fd = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())?;
    Ok(Generic::new(
        fd,
        calloop::Interest::READ,
        calloop::Mode::Level,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn owned_child_exit_wakes_once_and_removes_readable_pidfd() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let source = source(child.id()).unwrap();
        let mut loop_ = calloop::EventLoop::<usize>::try_new().unwrap();
        loop_
            .handle()
            .insert_source(source, |_, _, count| {
                *count += 1;
                Ok(calloop::PostAction::Remove)
            })
            .unwrap();
        let mut count = 0;
        loop_.dispatch(Some(Duration::ZERO), &mut count).unwrap();
        assert_eq!(count, 0, "live child is not readiness work");
        child.kill().unwrap();
        loop_
            .dispatch(Some(Duration::from_secs(2)), &mut count)
            .unwrap();
        assert_eq!(count, 1);
        child.wait().unwrap();
        loop_.dispatch(Some(Duration::ZERO), &mut count).unwrap();
        assert_eq!(count, 1, "exited child source is removed");
    }
    #[test]
    fn invalid_pid_cannot_select_an_unowned_process() {
        assert!(source(0).is_err());
        assert!(source(u32::MAX).is_err());
    }
}
