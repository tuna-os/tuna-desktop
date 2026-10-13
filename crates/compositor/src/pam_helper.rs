//! Private, authentication-only PAM conversation process.
//!
//! An authenticated result is provisional until the owned helper and its process
//! group retire. Keep a quarantined supervisor alive; a deadline is not retirement.
//! Detached module processes/external daemon state require separate stack proof.
use crate::pam_cancel::Cancellation;
use serde::{Deserialize, Serialize};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitCode, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const FLAG: &str = "--internal-pam-conversation";
const FRAME_LIMIT: usize = 64 * 1024;
const TEXT_LIMIT: usize = 4096;
const MESSAGE_LIMIT: usize = 32;
const ROUND_LIMIT: u32 = 64;
const MAX_BUDGET: Duration = Duration::from_secs(10);
const CLEANUP_BUDGET: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub nonce: [u8; 16],
    pub request: u64,
    pub generation: u64,
    pub uid: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Style {
    Visible,
    Secret,
    Info,
    Error,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub index: u16,
    pub style: Style,
    pub text: String,
}

impl std::fmt::Debug for Message {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("Message")
            .field("index", &self.index)
            .field("style", &self.style)
            .field("text", &"<redacted>")
            .finish()
    }
}

/// Answer values never format their contents and wipe their owned buffer on drop.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct AnswerText(String);
impl AnswerText {
    pub fn new(text: String) -> Self {
        Self(text)
    }
}
impl std::fmt::Debug for AnswerText {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str("AnswerText(<redacted>)")
    }
}
impl Drop for AnswerText {
    fn drop(&mut self) {
        // SAFETY: every byte is replaced with a valid UTF-8 NUL byte.
        unsafe { wipe(self.0.as_mut_vec()) };
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Answer {
    pub index: u16,
    pub text: Option<AnswerText>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Start {
        identity: Identity,
        service: String,
        user: String,
        budget_ms: u64,
    },
    Answers {
        identity: Identity,
        sequence: u32,
        answers: Vec<Answer>,
    },
    Cancel {
        identity: Identity,
    },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Challenge {
        identity: Identity,
        sequence: u32,
        messages: Vec<Message>,
    },
    Result {
        identity: Identity,
        authenticated: bool,
    },
}

struct Bytes(Vec<u8>);
impl Drop for Bytes {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}
pub(crate) fn wipe(bytes: &mut [u8]) {
    for byte in bytes {
        // SAFETY: this writes exclusively owned bytes; volatile prevents elision.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
}
fn invalid() -> io::Error {
    // Do not attach serde/PAM strings or any input to transport errors.
    io::Error::new(io::ErrorKind::InvalidData, "private PAM exchange refused")
}
fn nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: fcntl does not retain the descriptor or pointers.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
fn ready(fd: RawFd, events: i16, deadline: Instant) -> io::Result<()> {
    ready_with_cancel(fd, events, deadline, None)
}
fn ready_with_cancel(
    fd: RawFd,
    events: i16,
    deadline: Instant,
    cancel: Option<&Cancellation>,
) -> io::Result<()> {
    loop {
        if cancel.is_some_and(Cancellation::is_canceled) {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(io::ErrorKind::TimedOut)?;
        let mut polls = [
            libc::pollfd {
                fd,
                events,
                revents: 0,
            },
            libc::pollfd {
                fd: cancel.and_then(Cancellation::fd).unwrap_or(-1),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        if cancel.is_some() && polls[1].fd < 0 {
            return Err(invalid());
        }
        let count = if cancel.is_some() { 2 } else { 1 };
        let millis = remaining
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        // SAFETY: valid borrowed descriptors and array storage for this call.
        let result = unsafe { libc::poll(polls.as_mut_ptr(), count, millis) };
        if result > 0 {
            if let Some(cancel) = cancel {
                if cancel.is_canceled() {
                    return Err(io::ErrorKind::ConnectionAborted.into());
                }
                if polls[1].revents != 0 {
                    if polls[1].revents & libc::POLLIN == 0 {
                        return Err(invalid());
                    }
                    cancel.drain()?;
                    if cancel.is_canceled() {
                        return Err(io::ErrorKind::ConnectionAborted.into());
                    }
                }
            }
            if polls[0].revents != 0 {
                return Ok(());
            }
            // A stale wake is only notification; the current job remains authoritative.
            continue;
        }
        if result == 0 {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
fn read_exact<T: Read + AsRawFd>(
    stream: &mut T,
    bytes: &mut [u8],
    deadline: Instant,
    cancel: Option<&Cancellation>,
) -> io::Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        ready_with_cancel(stream.as_raw_fd(), libc::POLLIN, deadline, cancel)?;
        match stream.read(&mut bytes[offset..]) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => offset += count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
fn send<T: Write + AsRawFd, V: Serialize>(
    stream: &mut T,
    value: &V,
    deadline: Instant,
) -> io::Result<()> {
    send_with_cancel(stream, value, deadline, None)
}
fn send_with_cancel<T: Write + AsRawFd, V: Serialize>(
    stream: &mut T,
    value: &V,
    deadline: Instant,
    cancel: Option<&Cancellation>,
) -> io::Result<()> {
    let body = Bytes(serde_json::to_vec(value).map_err(|_| invalid())?);
    if body.0.is_empty() || body.0.len() > FRAME_LIMIT {
        return Err(invalid());
    }
    let header = (body.0.len() as u32).to_le_bytes();
    for part in [&header[..], &body.0[..]] {
        let mut offset = 0;
        while offset < part.len() {
            ready_with_cancel(stream.as_raw_fd(), libc::POLLOUT, deadline, cancel)?;
            match stream.write(&part[offset..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => offset += count,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}
fn receive<T: Read + AsRawFd, V: serde::de::DeserializeOwned>(
    stream: &mut T,
    deadline: Instant,
) -> io::Result<V> {
    receive_with_cancel(stream, deadline, None)
}
fn receive_with_cancel<T: Read + AsRawFd, V: serde::de::DeserializeOwned>(
    stream: &mut T,
    deadline: Instant,
    cancel: Option<&Cancellation>,
) -> io::Result<V> {
    let mut header = [0; 4];
    read_exact(stream, &mut header, deadline, cancel)?;
    let count = u32::from_le_bytes(header) as usize;
    if count == 0 || count > FRAME_LIMIT {
        return Err(invalid());
    }
    let mut body = Bytes(vec![0; count]);
    read_exact(stream, &mut body.0, deadline, cancel)?;
    serde_json::from_slice(&body.0).map_err(|_| invalid())
}

#[derive(Debug)]
pub enum Event {
    Challenge {
        sequence: u32,
        messages: Vec<Message>,
    },
    Finished {
        authenticated: bool,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retirement {
    Running,
    Retired,
    Quarantined,
}

/// A held reference to this running compositor image, never a caller path.
#[derive(Clone)]
pub struct SelfImage {
    file: std::sync::Arc<std::fs::File>,
}
impl SelfImage {
    pub fn capture() -> io::Result<Self> {
        let file = std::fs::File::open("/proc/self/exe")?;
        if !file.metadata()?.is_file() {
            return Err(invalid());
        }
        Ok(Self {
            file: std::sync::Arc::new(file),
        })
    }
    /// Retain this image and the supervisor on the creator through retirement.
    /// Exec resolves the derived fd path before CLOEXEC closes its descriptor.
    /// Actual replacement/unlink behavior remains an explicit CI proof obligation.
    pub fn spawn(
        &self,
        identity: Identity,
        service: String,
        user: String,
        budget: Duration,
    ) -> io::Result<Supervisor> {
        if budget.is_zero() || budget > MAX_BUDGET {
            return Err(invalid());
        }
        let executable =
            std::path::PathBuf::from(format!("/proc/self/fd/{}", self.file.as_raw_fd()));
        Supervisor::spawn_inner(
            &executable,
            identity,
            service,
            user,
            budget,
            Some(self.file.clone()),
            None,
        )
    }
    pub fn spawn_cancellable(
        &self,
        identity: Identity,
        service: String,
        user: String,
        budget: Duration,
        cancel: Cancellation,
    ) -> io::Result<Supervisor> {
        if budget.is_zero() || budget > MAX_BUDGET {
            return Err(invalid());
        }
        let executable =
            std::path::PathBuf::from(format!("/proc/self/fd/{}", self.file.as_raw_fd()));
        Supervisor::spawn_inner(
            &executable,
            identity,
            service,
            user,
            budget,
            Some(self.file.clone()),
            Some(cancel),
        )
    }
    pub(crate) fn spawn_conversation(
        &self,
        identity: Identity,
        service: String,
        user: String,
        total: Duration,
        cancel: Cancellation,
    ) -> io::Result<Supervisor> {
        let executable =
            std::path::PathBuf::from(format!("/proc/self/fd/{}", self.file.as_raw_fd()));
        Supervisor::spawn_inner(
            &executable,
            identity,
            service,
            user,
            total,
            Some(self.file.clone()),
            Some(cancel),
        )
    }
}

/// Blocking foundation API: call off the UI loop, retain ownership through retirement.
/// The password backend uses this helper. Interactive UI routing is separate.
pub struct Supervisor {
    child: Child,
    pidfd: Option<OwnedFd>,
    startup_error: Option<io::Error>,
    input: Option<ChildStdin>,
    output: Option<ChildStdout>,
    identity: Identity,
    deadline: Instant,
    total_deadline: Instant,
    sequence: u32,
    pending: Vec<Style>,
    retirement: Retirement,
    _source_image: Option<std::sync::Arc<std::fs::File>>,
    cancellation: Option<Cancellation>,
}
impl Supervisor {
    /// Trusted caller/test boundary: `executable` must be an independently admitted
    /// compositor executable. Private IPC does not authenticate an arbitrary program
    /// supplied here. Future live adoption must re-execute the held compositor image
    /// (for example `/proc/self/exe`), never a UI/environment/configuration path.
    /// The creating thread must stay alive through helper retirement: Linux's
    /// parent-death signal follows that thread, not the process. Use a stable
    /// supervisor owner, never a transient spawn-and-return thread.
    /// The budget bounds nonblocking IPC after launch; Command::spawn's OS exec
    /// handshake itself cannot be given a portable hard deadline here.
    pub fn spawn(
        executable: &std::path::Path,
        identity: Identity,
        service: String,
        user: String,
        budget: Duration,
    ) -> io::Result<Self> {
        if budget.is_zero() || budget > MAX_BUDGET {
            return Err(invalid());
        }
        Self::spawn_inner(executable, identity, service, user, budget, None, None)
    }
    /// Same trusted executable boundary; cancellation must be bound by its owner.
    pub fn spawn_cancellable(
        executable: &std::path::Path,
        identity: Identity,
        service: String,
        user: String,
        budget: Duration,
        cancel: Cancellation,
    ) -> io::Result<Self> {
        if budget.is_zero() || budget > MAX_BUDGET {
            return Err(invalid());
        }
        Self::spawn_inner(
            executable,
            identity,
            service,
            user,
            budget,
            None,
            Some(cancel),
        )
    }
    fn spawn_inner(
        executable: &std::path::Path,
        identity: Identity,
        service: String,
        user: String,
        budget: Duration,
        source_image: Option<std::sync::Arc<std::fs::File>>,
        cancellation: Option<Cancellation>,
    ) -> io::Result<Self> {
        if budget.as_millis() == 0
            || budget > crate::pam_conversation::MAX_TOTAL
            || identity.uid != unsafe { libc::getuid() }
            || service.is_empty()
            || service.len() > 128
            || user.len() > TEXT_LIMIT
            || !service
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(invalid());
        }
        let deadline = Instant::now() + budget;
        let parent = unsafe { libc::getpid() };
        let mut command = Command::new(executable);
        command
            .arg(FLAG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // SAFETY: pre_exec uses only async-signal-safe libc operations, no allocation.
        unsafe {
            command.pre_exec(move || {
                if libc::setpgid(0, 0) != 0
                    || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                    || libc::getppid() != parent
                {
                    return Err(io::Error::from_raw_os_error(libc::ECHILD));
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        let input = child.stdin.take();
        let output = child.stdout.take();
        // Ownership exists immediately after spawn, including every setup failure.
        let mut supervisor = Self {
            child,
            pidfd: None,
            startup_error: None,
            input,
            output,
            identity,
            deadline,
            total_deadline: deadline,
            sequence: 0,
            pending: Vec::new(),
            retirement: Retirement::Running,
            _source_image: source_image,
            cancellation,
        };
        let setup: io::Result<()> = (|| {
            let pid =
                rustix::process::Pid::from_raw(supervisor.child.id() as i32).ok_or_else(invalid)?;
            supervisor.pidfd = Some(rustix::process::pidfd_open(
                pid,
                rustix::process::PidfdFlags::empty(),
            )?);
            let input = supervisor.input.as_mut().ok_or_else(invalid)?;
            let output = supervisor.output.as_mut().ok_or_else(invalid)?;
            nonblocking(input.as_raw_fd())?;
            nonblocking(output.as_raw_fd())?;
            send_with_cancel(
                input,
                &Request::Start {
                    identity: supervisor.identity.clone(),
                    service,
                    user,
                    budget_ms: budget.as_millis() as u64,
                },
                deadline.min(Instant::now() + MAX_BUDGET),
                supervisor.cancellation.as_ref(),
            )
        })();
        if let Err(error) = setup {
            supervisor.startup_error = Some(error);
            supervisor.cancel();
        }
        // An initialized but failed helper is still returned as an owned object.
        // next_event reports its failure; quarantine is never discarded here.
        Ok(supervisor)
    }
    /// Trusted factory seam for explicit interactive method policies. Unlike
    /// legacy spawn, the total ceiling is not the ten-second password budget.
    /// Only the held-self live factory uses this for production conversations.
    pub fn spawn_conversation(
        executable: &std::path::Path,
        identity: Identity,
        service: String,
        user: String,
        total: Duration,
        cancel: Cancellation,
    ) -> io::Result<Self> {
        Self::spawn_inner(
            executable,
            identity,
            service,
            user,
            total,
            None,
            Some(cancel),
        )
    }
    /// Change only the parent's bounded IPC phase, never the child's explicit
    /// absolute total ceiling, identity, sequence or physical retirement state.
    pub(crate) fn set_phase_deadline(&mut self, deadline: Instant) -> io::Result<()> {
        if deadline <= Instant::now() || self.retirement != Retirement::Running {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.deadline = deadline.min(self.total_deadline);
        Ok(())
    }
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
    pub fn next_event(&mut self) -> io::Result<Event> {
        if let Some(error) = self.startup_error.take() {
            self.cancel();
            return Err(error);
        }
        if !self.pending.is_empty() {
            self.cancel();
            return Err(invalid());
        }
        let result = self.read_event();
        if result.is_err() {
            self.cancel();
        }
        result
    }
    fn read_event(&mut self) -> io::Result<Event> {
        match receive_with_cancel(
            self.output.as_mut().ok_or_else(invalid)?,
            self.deadline,
            self.cancellation.as_ref(),
        )? {
            Reply::Challenge {
                identity,
                sequence,
                messages,
            } => {
                if identity != self.identity
                    || sequence != self.sequence + 1
                    || sequence > ROUND_LIMIT
                    || messages.is_empty()
                    || messages.len() > MESSAGE_LIMIT
                {
                    return Err(invalid());
                }
                for (index, message) in messages.iter().enumerate() {
                    if message.index as usize != index
                        || message.text.len() > TEXT_LIMIT
                        || message.text.contains('\0')
                    {
                        return Err(invalid());
                    }
                }
                self.sequence = sequence;
                self.pending = messages.iter().map(|message| message.style).collect();
                Ok(Event::Challenge { sequence, messages })
            }
            Reply::Result {
                identity,
                authenticated,
            } => {
                if identity != self.identity {
                    return Err(invalid());
                }
                self.input.take();
                self.output.take();
                if self.retire(false) != Retirement::Retired {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                let successful_exit = self
                    .child
                    .try_wait()?
                    .is_some_and(|status| status.success());
                Ok(Event::Finished {
                    authenticated: authenticated && successful_exit,
                })
            }
        }
    }
    pub fn answer(&mut self, sequence: u32, answers: Vec<Answer>) -> io::Result<()> {
        if sequence != self.sequence || !valid_answers(&self.pending, &answers) {
            self.cancel();
            return Err(invalid());
        }
        let request = Request::Answers {
            identity: self.identity.clone(),
            sequence,
            answers,
        };
        let result = send_with_cancel(
            self.input.as_mut().ok_or_else(invalid)?,
            &request,
            self.deadline,
            self.cancellation.as_ref(),
        );
        if result.is_ok() {
            self.pending.clear();
        } else {
            self.cancel();
        }
        result
    }
    pub fn cancel(&mut self) -> Retirement {
        if let Some(input) = &mut self.input {
            let _ = send(
                input,
                &Request::Cancel {
                    identity: self.identity.clone(),
                },
                Instant::now() + Duration::from_millis(10),
            );
        }
        self.input.take();
        self.output.take();
        self.pending.clear();
        self.retire(true)
    }
    pub fn retry_retirement(&mut self) -> Retirement {
        self.retire(true)
    }
    pub fn retirement(&self) -> Retirement {
        self.retirement
    }
    fn retire(&mut self, kill: bool) -> Retirement {
        if self.retirement == Retirement::Retired {
            return self.retirement;
        }
        if kill {
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
            let _ = self.child.kill();
        }
        let deadline = Instant::now() + CLEANUP_BUDGET;
        // The unreaped leader reserves its numeric identity while the group is
        // observed. No signal is sent to a group after reaping its leader.
        let exited = self
            .pidfd
            .as_ref()
            .is_some_and(|fd| ready(fd.as_raw_fd(), libc::POLLIN, deadline).is_ok());
        if exited || self.pidfd.is_none() {
            while Instant::now() < deadline {
                match group_has_no_live_members(self.child.id(), deadline) {
                    Ok(true) => {
                        if self.child.try_wait().ok().flatten().is_some() {
                            self.retirement = Retirement::Retired;
                            break;
                        }
                    }
                    Ok(false) => {}
                    Err(_) => break,
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        if self.retirement != Retirement::Retired {
            self.retirement = Retirement::Quarantined;
        }
        self.retirement
    }
}
impl Drop for Supervisor {
    fn drop(&mut self) {
        // This is shutdown cleanup, not a positive retirement receipt.
        if self.retirement != Retirement::Retired {
            self.retire(true);
        }
    }
}
fn group_has_no_live_members(group: u32, deadline: Instant) -> io::Result<bool> {
    let mut entries = 0;
    for entry in std::fs::read_dir("/proc")? {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let entry = entry?;
        if !entry
            .file_name()
            .as_encoded_bytes()
            .iter()
            .all(u8::is_ascii_digit)
        {
            continue;
        }
        entries += 1;
        if entries > 32768 {
            return Err(invalid());
        }
        let bytes = match std::fs::read(entry.path().join("stat")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if bytes.len() > 4096 {
            return Err(invalid());
        }
        // A process name can contain arbitrary bytes. Only the kernel's ASCII
        // state/PPID/PGRP tail matters; unrelated names cannot poison retirement.
        let end = bytes
            .iter()
            .rposition(|byte| *byte == b')')
            .ok_or_else(invalid)?;
        let tail = std::str::from_utf8(&bytes[end + 1..]).map_err(|_| invalid())?;
        let fields: Vec<_> = tail.split_whitespace().take(3).collect();
        if fields.len() != 3 {
            return Err(invalid());
        }
        let pgrp: u32 = fields[2].parse().map_err(|_| invalid())?;
        if pgrp == group && !matches!(fields[0], "Z" | "X") {
            return Ok(false);
        }
    }
    Ok(true)
}
pub(crate) fn valid_answers(styles: &[Style], answers: &[Answer]) -> bool {
    !styles.is_empty()
        && styles.len() == answers.len()
        && answers.iter().enumerate().all(|(index, answer)| {
            answer.index as usize == index
                && match styles[index] {
                    Style::Visible | Style::Secret => answer.text.as_ref().is_some_and(|value| {
                        value.0.len() <= TEXT_LIMIT && !value.0.contains('\0')
                    }),
                    Style::Info | Style::Error => answer.text.is_none(),
                }
        })
}

#[repr(C)]
struct PamMessage {
    style: c_int,
    text: *const c_char,
}
#[repr(C)]
struct PamResponse {
    text: *mut c_char,
    code: c_int,
}
type Conversation = unsafe extern "C" fn(
    c_int,
    *mut *const PamMessage,
    *mut *mut PamResponse,
    *mut c_void,
) -> c_int;
#[repr(C)]
struct PamConv {
    callback: Conversation,
    data: *mut c_void,
}
type Start =
    unsafe extern "C" fn(*const c_char, *const c_char, *const PamConv, *mut *mut c_void) -> c_int;
type PamFn = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type GetItem = unsafe extern "C" fn(*const c_void, c_int, *mut *const c_void) -> c_int;
struct Exchange {
    input: std::fs::File,
    output: std::fs::File,
    identity: Identity,
    deadline: Instant,
    sequence: u32,
}
unsafe extern "C" fn conversation(
    count: c_int,
    messages: *mut *const PamMessage,
    responses: *mut *mut PamResponse,
    data: *mut c_void,
) -> c_int {
    if count <= 0
        || count as usize > MESSAGE_LIMIT
        || messages.is_null()
        || responses.is_null()
        || data.is_null()
    {
        return 19;
    }
    *responses = std::ptr::null_mut();
    // A module can call its conversation on another thread. Reject concurrent
    // conversations instead of aliasing mutable IPC state or waiting forever.
    let exchange = &*(data as *const Mutex<Exchange>);
    let Ok(mut exchange) = exchange.try_lock() else {
        return 19;
    };
    // No unwinding across PAM's C ABI.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        exchange_messages(count as usize, messages, responses, &mut exchange)
    }))
    .unwrap_or(19)
}
unsafe fn exchange_messages(
    count: usize,
    incoming: *mut *const PamMessage,
    output: *mut *mut PamResponse,
    exchange: &mut Exchange,
) -> c_int {
    let mut messages = Vec::with_capacity(count);
    for index in 0..count {
        let message = *incoming.add(index);
        if message.is_null() || (*message).text.is_null() {
            return 19;
        }
        let len = libc::strnlen((*message).text, TEXT_LIMIT + 1);
        if len > TEXT_LIMIT {
            return 19;
        }
        let text = match std::str::from_utf8(std::slice::from_raw_parts(
            (*message).text.cast::<u8>(),
            len,
        )) {
            Ok(text) => text.to_owned(),
            Err(_) => return 19,
        };
        let style = match (*message).style {
            1 => Style::Secret,
            2 => Style::Visible,
            3 => Style::Error,
            4 => Style::Info,
            _ => return 19,
        };
        messages.push(Message {
            index: index as u16,
            style,
            text,
        });
    }
    exchange.sequence += 1;
    if exchange.sequence > ROUND_LIMIT {
        return 19;
    }
    let styles: Vec<_> = messages.iter().map(|message| message.style).collect();
    if send(
        &mut exchange.output,
        &Reply::Challenge {
            identity: exchange.identity.clone(),
            sequence: exchange.sequence,
            messages,
        },
        exchange.deadline,
    )
    .is_err()
    {
        return 19;
    }
    let answers = match receive(&mut exchange.input, exchange.deadline) {
        Ok(Request::Answers {
            identity,
            sequence,
            answers,
        }) if identity == exchange.identity
            && sequence == exchange.sequence
            && valid_answers(&styles, &answers) =>
        {
            answers
        }
        _ => return 19,
    };
    let out = libc::calloc(count, std::mem::size_of::<PamResponse>()).cast::<PamResponse>();
    if out.is_null() {
        return 19;
    }
    for (index, answer) in answers.iter().enumerate() {
        if let Some(value) = &answer.text {
            // Allocate directly; avoid an additional unwiped CString copy.
            let target = libc::malloc(value.0.len() + 1).cast::<c_char>();
            if target.is_null() {
                free_responses(out, index);
                return 19;
            }
            std::ptr::copy_nonoverlapping(value.0.as_ptr(), target.cast::<u8>(), value.0.len());
            *target.add(value.0.len()) = 0;
            (*out.add(index)).text = target;
        }
    }
    *output = out;
    0
}
unsafe fn free_responses(out: *mut PamResponse, filled: usize) {
    for index in 0..filled {
        let text = (*out.add(index)).text;
        if !text.is_null() {
            wipe(std::slice::from_raw_parts_mut(
                text.cast::<u8>(),
                libc::strlen(text),
            ));
            libc::free(text.cast());
        }
    }
    libc::free(out.cast());
}
fn run_helper() -> io::Result<bool> {
    let started = Instant::now();
    // SAFETY: the fixed early helper entry point exclusively owns these pipes.
    let mut input = unsafe { std::fs::File::from_raw_fd(0) };
    let output = unsafe { std::fs::File::from_raw_fd(1) };
    for descriptor in [input.as_raw_fd(), output.as_raw_fd()] {
        // Require the private pipe topology used by Supervisor, never a tty,
        // credential file or public diagnostic output stream.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(descriptor, &mut stat) } != 0
            || stat.st_mode & libc::S_IFMT != libc::S_IFIFO
            || !std::fs::read_link(format!("/proc/self/fd/{descriptor}"))?
                .as_os_str()
                .as_encoded_bytes()
                .starts_with(b"pipe:[")
        {
            return Err(invalid());
        }
        // Other exec'd PAM helpers must not inherit the conversation pipes.
        if unsafe { libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        nonblocking(descriptor)?;
    }
    let Request::Start {
        identity,
        service,
        user,
        budget_ms,
    } = receive(&mut input, started + MAX_BUDGET)?
    else {
        return Err(invalid());
    };
    let uid = unsafe { libc::getuid() };
    let account = unsafe { libc::getpwuid(uid) };
    if account.is_null()
        || uid != unsafe { libc::geteuid() }
        || identity.uid != uid
        || user.as_bytes() != unsafe { CStr::from_ptr((*account).pw_name) }.to_bytes()
        || !(1..=crate::pam_conversation::MAX_TOTAL.as_millis() as u64).contains(&budget_ms)
        || service.is_empty()
        || service.len() > 128
        || !service
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(invalid());
    }
    let service = CString::new(service).map_err(|_| invalid())?;
    let user = CString::new(user).map_err(|_| invalid())?;
    let exchange = Mutex::new(Exchange {
        input,
        output,
        identity,
        deadline: started + Duration::from_millis(budget_ms),
        sequence: 0,
    });
    use libloading::os::unix::{Library, RTLD_GLOBAL, RTLD_NOW};
    // SAFETY: signatures follow Linux-PAM; library remains alive through pam_end.
    let authenticated = unsafe {
        let _library =
            Library::open(Some("libpam.so.0"), RTLD_NOW | RTLD_GLOBAL).map_err(|_| invalid())?;
        let scope = Library::this();
        let start = scope.get::<Start>(b"pam_start\0").map_err(|_| invalid())?;
        let auth = scope
            .get::<PamFn>(b"pam_authenticate\0")
            .map_err(|_| invalid())?;
        let account = scope
            .get::<PamFn>(b"pam_acct_mgmt\0")
            .map_err(|_| invalid())?;
        let end = scope.get::<PamFn>(b"pam_end\0").map_err(|_| invalid())?;
        let get_item = scope
            .get::<GetItem>(b"pam_get_item\0")
            .map_err(|_| invalid())?;
        let conversation = PamConv {
            callback: conversation,
            data: (&exchange as *const Mutex<Exchange>).cast_mut().cast(),
        };
        let mut handle = std::ptr::null_mut();
        if start(service.as_ptr(), user.as_ptr(), &conversation, &mut handle) != 0
            || handle.is_null()
        {
            return Err(invalid());
        }
        let mut status = auth(handle, 0x8000);
        if status == 0 {
            status = account(handle, 0x8000);
        }
        let mut authenticated_user: *const c_void = std::ptr::null();
        let same_user = get_item(handle, 2, &mut authenticated_user) == 0
            && !authenticated_user.is_null()
            && libc::strnlen(authenticated_user.cast(), TEXT_LIMIT + 1) <= TEXT_LIMIT
            && CStr::from_ptr(authenticated_user.cast()).to_bytes() == user.as_bytes();
        let ended = end(handle, status);
        status == 0 && ended == 0 && same_user
    };
    let mut exchange = exchange.into_inner().map_err(|_| invalid())?;
    send(
        &mut exchange.output,
        &Reply::Result {
            identity: exchange.identity.clone(),
            authenticated,
        },
        exchange.deadline,
    )?;
    Ok(authenticated)
}
/// Fixed private entry point. Failure diagnostics intentionally contain no input.
pub fn entry() -> ExitCode {
    // Library/module panics must not write challenge/answer values to stderr.
    std::panic::set_hook(Box::new(|_| {}));
    let limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // Secrets must not be persisted by an abort/core-dumping PAM module.
    if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) } != 0 {
        return ExitCode::FAILURE;
    }
    if run_helper().is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;

    fn await_cancelable_poll(tid: libc::c_long) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let text = std::fs::read_to_string(format!("/proc/self/task/{tid}/syscall")).unwrap();
            let fields: Vec<_> = text.split_whitespace().collect();
            let syscall = fields.first().and_then(|v| v.parse::<libc::c_long>().ok());
            let polling = syscall == Some(libc::SYS_ppoll);
            #[cfg(target_arch = "x86_64")]
            let polling = polling || syscall == Some(libc::SYS_poll);
            if polling && fields.get(2).copied() == Some("0x2") {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("worker never entered the actual two-fd cancellation poll");
    }
    fn bound_cancel() -> Cancellation {
        let cancel = Cancellation::default();
        cancel
            .bind(
                crate::pam_cancel::WakeFd::new(Arc::new(std::sync::atomic::AtomicBool::new(false)))
                    .unwrap(),
            )
            .unwrap();
        cancel
    }
    #[test]
    fn private_pam_cancel_interrupts_idle_partial_header_and_partial_body() {
        for prefix in [vec![], vec![1, 0], vec![10, 0, 0, 0, b'{']] {
            let (mut input, mut peer) = UnixStream::pair().unwrap();
            nonblocking(input.as_raw_fd()).unwrap();
            peer.write_all(&prefix).unwrap();
            let cancel = bound_cancel();
            let reader_cancel = cancel.clone();
            let (entered, observed) = std::sync::mpsc::channel();
            let reader = std::thread::spawn(move || {
                entered
                    .send(unsafe { libc::syscall(libc::SYS_gettid) })
                    .unwrap();
                receive_with_cancel::<_, Request>(
                    &mut input,
                    Instant::now() + MAX_BUDGET,
                    Some(&reader_cancel),
                )
                .err()
                .unwrap()
                .kind()
            });
            await_cancelable_poll(observed.recv_timeout(Duration::from_secs(2)).unwrap());
            let started = Instant::now();
            cancel.cancel();
            assert_eq!(reader.join().unwrap(), io::ErrorKind::ConnectionAborted);
            assert!(started.elapsed() < Duration::from_secs(2));
            drop(peer);
        }
    }
    #[test]
    fn private_pam_old_attempt_wake_does_not_abort_a_fresh_read() {
        let wake =
            crate::pam_cancel::WakeFd::new(Arc::new(std::sync::atomic::AtomicBool::new(false)))
                .unwrap();
        let old = Cancellation::default();
        old.bind(wake.clone()).unwrap();
        let fresh = Cancellation::default();
        fresh.bind(wake).unwrap();
        old.cancel();
        let (mut input, mut peer) = UnixStream::pair().unwrap();
        nonblocking(input.as_raw_fd()).unwrap();
        let (entered, observed) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            entered
                .send(unsafe { libc::syscall(libc::SYS_gettid) })
                .unwrap();
            receive_with_cancel::<_, Request>(&mut input, Instant::now() + MAX_BUDGET, Some(&fresh))
        });
        await_cancelable_poll(observed.recv_timeout(Duration::from_secs(2)).unwrap());
        old.cancel();
        send(
            &mut peer,
            &Request::Cancel {
                identity: Identity {
                    nonce: [1; 16],
                    request: 1,
                    generation: 1,
                    uid: unsafe { libc::getuid() },
                },
            },
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap();
        assert!(matches!(
            reader.join().unwrap().unwrap(),
            Request::Cancel { .. }
        ));
    }
    #[test]
    fn private_pam_cancel_interrupts_blocked_start_and_answer_writes() {
        for request in [
            Request::Answers {
                identity: Identity {
                    nonce: [1; 16],
                    request: 1,
                    generation: 1,
                    uid: unsafe { libc::getuid() },
                },
                sequence: 1,
                answers: vec![Answer {
                    index: 0,
                    text: Some(AnswerText::new("controlled-only".into())),
                }],
            },
            Request::Start {
                identity: Identity {
                    nonce: [1; 16],
                    request: 1,
                    generation: 1,
                    uid: unsafe { libc::getuid() },
                },
                service: "controlled".into(),
                user: "controlled".into(),
                budget_ms: 10000,
            },
        ] {
            let (mut output, peer) = UnixStream::pair().unwrap();
            nonblocking(output.as_raw_fd()).unwrap();
            loop {
                match output.write(&[0; 4096]) {
                    Ok(n) => assert!(n > 0),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => panic!("socket fill failed: {e}"),
                }
            }
            let cancel = bound_cancel();
            let writer_cancel = cancel.clone();
            let (entered, observed) = std::sync::mpsc::channel();
            let writer = std::thread::spawn(move || {
                entered
                    .send(unsafe { libc::syscall(libc::SYS_gettid) })
                    .unwrap();
                send_with_cancel(
                    &mut output,
                    &request,
                    Instant::now() + MAX_BUDGET,
                    Some(&writer_cancel),
                )
                .unwrap_err()
                .kind()
            });
            await_cancelable_poll(observed.recv_timeout(Duration::from_secs(2)).unwrap());
            let started = Instant::now();
            cancel.cancel();
            assert_eq!(writer.join().unwrap(), io::ErrorKind::ConnectionAborted);
            assert!(started.elapsed() < Duration::from_secs(2));
            drop(peer);
        }
    }
    #[test]
    fn private_pam_codec_rejects_unknown_duplicate_oversized_and_redacts_errors() {
        for bytes in [
            br#"{"kind":"cancel","identity":{},"secret":"never-log-this"}"#.as_slice(),
            br#"{"kind":"cancel","kind":"start"}"#.as_slice(),
            br#"{"kind":"unrecognized","answer":"never-log-this"}"#.as_slice(),
        ] {
            let (mut input, mut output) = UnixStream::pair().unwrap();
            nonblocking(input.as_raw_fd()).unwrap();
            output
                .write_all(&(bytes.len() as u32).to_le_bytes())
                .unwrap();
            output.write_all(bytes).unwrap();
            let error = receive::<_, Request>(&mut input, Instant::now() + Duration::from_secs(1))
                .err()
                .unwrap();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(!error.to_string().contains("never-log-this"));
        }
        for count in [0, FRAME_LIMIT as u32 + 1, u32::MAX] {
            let (mut input, mut output) = UnixStream::pair().unwrap();
            output.write_all(&count.to_le_bytes()).unwrap();
            assert!(
                receive::<_, Request>(&mut input, Instant::now() + Duration::from_secs(1)).is_err()
            );
        }
    }
    #[test]
    fn private_pam_partial_frame_dribble_cannot_extend_absolute_budget() {
        let (mut input, mut output) = UnixStream::pair().unwrap();
        nonblocking(input.as_raw_fd()).unwrap();
        output.write_all(&1000u32.to_le_bytes()).unwrap();
        let writer = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(150);
            while Instant::now() < deadline {
                if output.write_all(b" ").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        let started = Instant::now();
        let result = receive::<_, Request>(&mut input, started + Duration::from_millis(40));
        drop(input);
        writer.join().unwrap();
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn private_pam_indexed_answers_and_diagnostics_are_separate_and_bounded() {
        let styles = [Style::Visible, Style::Secret, Style::Info, Style::Error];
        let mut answers = vec![
            Answer {
                index: 0,
                text: Some(AnswerText::new("name".into())),
            },
            Answer {
                index: 1,
                text: Some(AnswerText::new("never-log-this".into())),
            },
            Answer {
                index: 2,
                text: None,
            },
            Answer {
                index: 3,
                text: None,
            },
        ];
        assert!(valid_answers(&styles, &answers));
        assert!(!format!("{answers:?}").contains("never-log-this"));
        let message = Message {
            index: 0,
            style: Style::Error,
            text: "never-log-this".into(),
        };
        assert!(!format!("{message:?}").contains("never-log-this"));
        answers[2].text = Some(AnswerText::new("notice is not a prompt".into()));
        assert!(!valid_answers(&styles, &answers));
        answers[2].text = None;
        answers[1].index = 0;
        assert!(!valid_answers(&styles, &answers));
        answers[1].index = 1;
        answers[1].text = Some(AnswerText::new("x".repeat(TEXT_LIMIT + 1)));
        assert!(!valid_answers(&styles, &answers));
        let mut bytes = b"never-log-this".to_vec();
        wipe(&mut bytes);
        assert!(bytes.iter().all(|byte| *byte == 0));
    }
    #[test]
    fn private_pam_live_owned_process_is_quarantined_until_confirmed_retirement() {
        let mut child = Command::new("/bin/sleep")
            .arg("30")
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = rustix::process::Pid::from_raw(child.id() as i32).unwrap();
        let pidfd = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()).unwrap();
        let input = child.stdin.take();
        let output = child.stdout.take();
        let mut owner = Supervisor {
            child,
            pidfd: Some(pidfd),
            startup_error: None,
            input,
            output,
            identity: Identity {
                nonce: [1; 16],
                request: 1,
                generation: 0,
                uid: unsafe { libc::getuid() },
            },
            deadline: Instant::now() + MAX_BUDGET,
            total_deadline: Instant::now() + MAX_BUDGET,
            sequence: 0,
            pending: Vec::new(),
            retirement: Retirement::Running,
            _source_image: None,
            cancellation: None,
        };
        assert_eq!(owner.retire(false), Retirement::Quarantined);
        assert!(std::path::Path::new(&format!("/proc/{}", owner.pid())).exists());
        let deadline = Instant::now() + Duration::from_secs(2);
        while owner.cancel() != Retirement::Retired && Instant::now() < deadline {}
        assert_eq!(owner.retirement(), Retirement::Retired);
        assert!(!std::path::Path::new(&format!("/proc/{}", owner.pid())).exists());
    }
}
