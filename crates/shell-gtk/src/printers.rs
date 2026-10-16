//! Printers panel (P-ST-10 subset of #362): the printers CUPS knows,
//! with status, ink levels, jobs and the actions GNOME 51 offers.
//!
//! GNOME 51's printers panel (`panels/printers/` at control-center
//! 51.0, commit `6ae712c0454f2586f00821c3486150c1e8feb3e7`) reads
//! printers and jobs over IPP through libcups and performs privileged
//! writes (add, remove, default, enable, options) through the
//! `org.opensuse.CupsPkHelper.Mechanism` system service. This module
//! mirrors that split with no new dependencies:
//!
//! - [`CupsClient`] speaks IPP/1.1 over HTTP to the CUPS daemon
//!   (its Unix socket, else TCP `localhost:631`, else `$CUPS_SERVER`),
//!   using only `std::net`. Operation, tag, status and state numbers
//!   are those of `cups/ipp.h` in openprinting/cups.
//! - [`PrintersPanel`] renders the panel: one entry per printer with
//!   Model, Location, Status and Ink Level rows, the error banner and
//!   Restart action for stopped printers, the Active Jobs list with
//!   per-job Cancel and Clear All, and Test Page, Clean Print Heads,
//!   Use Printer by Default and Remove Printer actions. Strings follow
//!   the panel's own Blueprint sources (see
//!   `tests/settings/gnome51-control-center-items.json`).
//!
//! CUPS I/O runs on worker threads with a 2s connect timeout and 5s
//! per-read/per-write timeouts, plus a 10s absolute deadline per
//! exchange and a 30s budget per snapshot/action, so an absent or
//! stuck daemon cannot block the UI thread. Each connect/read/write
//! uses the exchange's remaining budget (capped by its per-call
//! limit), with no detached timeout thread: Unix connects wait on
//! `poll` (a full backlog times out), hostname resolution runs
//! `getent` under kill-and-reap, and a peer that dribbles bytes fails
//! by the deadline, each releasing its worker. The local `lpoptions`
//! default is a bounded regular-file descriptor read (FIFO and huge
//! files fall through to the server). A missing daemon renders as
//! GNOME's "No Print Service". Privileged writes are attempted as
//! direct IPP and, when cupsd answers 401 or 403, surface as
//! "Authentication Required": a cups-pk-helper mechanism call is the
//! follow-up, not invented here.

use std::cell::{Cell, RefCell};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk4 as gtk;

use crate::services::Tile;

/// IPP version we speak. CUPS 2.x servers all accept 1.1; nothing here
/// needs 2.0 features.
const IPP_MAJOR: u8 = 1;
const IPP_MINOR: u8 = 1;

/// Attribute group tags (`cups/ipp.h` `ipp_tag_t`).
const TAG_OPERATION: u8 = 0x01;
const TAG_JOB: u8 = 0x02;
const TAG_END: u8 = 0x03;
const TAG_PRINTER: u8 = 0x04;

/// Value tags we encode or decode.
const TAG_INTEGER: u8 = 0x21;
const TAG_BOOLEAN: u8 = 0x22;
const TAG_ENUM: u8 = 0x23;
const TAG_OCTET_STRING: u8 = 0x30;
const TAG_RANGE: u8 = 0x33;
const TAG_BEGIN_COLLECTION: u8 = 0x34;
const TAG_TEXT_WITH_LANGUAGE: u8 = 0x35;
const TAG_NAME_WITH_LANGUAGE: u8 = 0x36;
const TAG_END_COLLECTION: u8 = 0x37;
const TAG_TEXT: u8 = 0x41;
const TAG_NAME: u8 = 0x42;
// 0x43 is IPP_TAG_RESERVED_STRING and 0x46 IPP_TAG_URISCHEME: no
// request or response value uses either, so neither has a constant.
const TAG_KEYWORD: u8 = 0x44;
const TAG_URI: u8 = 0x45;
const TAG_CHARSET: u8 = 0x47;
const TAG_LANGUAGE: u8 = 0x48;
const TAG_MIME: u8 = 0x49;

/// Operations used (`cups/ipp.h` `ipp_op_t`). Note `CUPS-Set-Default`
/// is `0x400A`, not `0x4001` (`CUPS-Get-Default`).
const OP_PRINT_JOB: u16 = 0x0002;
const OP_CANCEL_JOB: u16 = 0x0008;
const OP_GET_JOBS: u16 = 0x000A;
// Only `printer_details` sends this operation (see its note).
#[allow(dead_code)]
const OP_GET_PRINTER_ATTRIBUTES: u16 = 0x000B;
const OP_PAUSE_PRINTER: u16 = 0x0010;
const OP_RESUME_PRINTER: u16 = 0x0011;
const OP_CUPS_GET_DEFAULT: u16 = 0x4001;
const OP_CUPS_GET_PRINTERS: u16 = 0x4002;
const OP_CUPS_DELETE_PRINTER: u16 = 0x4004;
const OP_CUPS_SET_DEFAULT: u16 = 0x400A;

/// Status codes we map (`cups/ipp.h` `ipp_status_t`).
const STATUS_OK: u16 = 0x0000;
const STATUS_FORBIDDEN: u16 = 0x0401;
const STATUS_NOT_AUTHENTICATED: u16 = 0x0402;
const STATUS_NOT_AUTHORIZED: u16 = 0x0403;
const STATUS_NOT_FOUND: u16 = 0x0406;

/// Printer states (`ipp_pstate_t`): idle, processing, stopped.
const PSTATE_IDLE: i32 = 3;
const PSTATE_PROCESSING: i32 = 4;
const PSTATE_STOPPED: i32 = 5;

/// Job states (`ipp_jstate_t`): pending, held, processing, stopped,
/// canceled, aborted, completed.
const JSTATE_PENDING: i32 = 3;
const JSTATE_COMPLETED: i32 = 9;

/// CUPS document format for maintenance commands (Print-Job with the
/// command as the body), as `pp-maintenance-command.c` sends them.
const FORMAT_CUPS_COMMAND: &str = "application/vnd.cups-command";
/// Let cupsd sniff the type of a test-page file.
const FORMAT_AUTO: &str = "application/octet-stream";

/// Unix sockets cupsd listens on, in probe order.
const CUPS_SOCKETS: [&str; 2] = ["/run/cups/cups.sock", "/var/run/cups/cups.sock"];
/// TCP fallback when no socket answers.
const CUPS_TCP_HOST: &str = "127.0.0.1";
const CUPS_TCP_PORT: u16 = 631;

/// Per-call bounds: each connect fails after 2s and each read/write
/// after 5s. Absolute deadlines (below) cap the whole exchange and
/// the whole snapshot/action via remaining-time timeouts.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const IO_TIMEOUT: Duration = Duration::from_secs(5);
/// Absolute deadline per CUPS exchange: connect, request write and
/// response head/body reads together must finish within 10s.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(10);
/// Whole-worker budget for a snapshot or multi-exchange action: the
/// serial per-printer job queries within one refresh share 30s.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(30);
const ACTION_TIMEOUT: Duration = Duration::from_secs(30);
/// Cap on an IPP response body (printers and jobs lists are small).
const MAX_BODY: usize = 8 << 20;
/// Cap on `~/.cups/lpoptions`: a few text lines, never megabytes. The
/// bounded read refuses anything larger (falls through to the server
/// default) rather than blocking on a huge file.
const LPOPTIONS_MAX: usize = 64 * 1024;

/// A worker released by its deadline: retryable transport failure.
fn deadline_error() -> CupsError {
    CupsError::new(
        CupsErrorKind::Transport,
        "print server exchange exceeded its deadline",
    )
}

/// Remaining budget capped by the per-call limit: every connect/read/
/// write sets its socket timeout to this, so a dribbling peer fails by
/// `deadline` even when each call is under its own limit.
fn remaining(deadline: Instant, cap: Duration) -> Result<Duration, CupsError> {
    let now = Instant::now();
    if now >= deadline {
        return Err(deadline_error());
    }
    Ok((deadline - now).min(cap))
}

/// One exchange inside an outer snapshot/action budget: bounded by the
/// outer deadline and by a fresh per-exchange timeout, whichever ends
/// first.
fn effective_exchange_deadline(outer: Instant, exchange_timeout: Duration) -> Instant {
    outer.min(Instant::now() + exchange_timeout)
}

/// Connect to one Unix socket within `timeout`, including a full
/// listen backlog: nonblocking `connect` plus `poll` for writability,
/// then back to blocking for the timeout-based reads and writes. No
/// spawned thread: the wait runs on the calling worker.
fn unix_connect_with_timeout(
    path: &Path,
    timeout: Duration,
) -> std::io::Result<std::os::unix::net::UnixStream> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::FromRawFd;
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let close_and = |err: std::io::Error| -> std::io::Result<std::os::unix::net::UnixStream> {
        unsafe {
            libc::close(fd);
        }
        Err(err)
    };
    let bytes = path.as_os_str().as_bytes();
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    if bytes.len() >= addr.sun_path.len() {
        return close_and(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "unix socket path too long",
        ));
    }
    unsafe {
        std::ptr::copy_nonoverlapping(
            bytes.as_ptr(),
            addr.sun_path.as_mut_ptr() as *mut u8,
            bytes.len(),
        );
    }
    let deadline = Instant::now() + timeout;
    let timed_out = || std::io::Error::new(std::io::ErrorKind::TimedOut, "unix connect timed out");
    'connect: loop {
        let ret = unsafe {
            libc::connect(
                fd,
                &addr as *const libc::sockaddr_un as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
            )
        };
        if ret == 0 {
            break;
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::EAGAIN) {
            // Full listen backlog on a nonblocking Unix socket: no
            // attempt is pending, so retry the connect itself until
            // the budget ends.
            if Instant::now() >= deadline {
                return close_and(timed_out());
            }
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        if err.raw_os_error() != Some(libc::EINPROGRESS) {
            return close_and(err);
        }
        loop {
            let now = Instant::now();
            if now >= deadline {
                return close_and(timed_out());
            }
            let ms = (deadline - now).as_millis().min(i32::MAX as u128) as i32;
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut pfd, 1, ms) };
            if ready == 0 {
                return close_and(timed_out());
            }
            if ready < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return close_and(err);
            }
            let mut so_error: libc::c_int = 0;
            let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
            let got = unsafe {
                libc::getsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    &mut so_error as *mut libc::c_int as *mut libc::c_void,
                    &mut len,
                )
            };
            if got != 0 {
                return close_and(std::io::Error::last_os_error());
            }
            if so_error != 0 {
                return close_and(std::io::Error::from_raw_os_error(so_error));
            }
            break 'connect;
        }
    }
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return close_and(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) } < 0 {
        return close_and(std::io::Error::last_os_error());
    }
    Ok(unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) })
}

/// Run `cmd args` and capture its stdout, giving up by `deadline` or
/// `budget` (whichever ends first): the child is killed and reaped
/// before returning, so no spawned work outlives the exchange and no
/// thread is involved. The wait polls on the calling worker.
fn run_command_with_budget(
    cmd: &str,
    args: &[&str],
    deadline: Instant,
    budget: Duration,
) -> Result<String, String> {
    use std::process::Stdio;
    let mut child = std::process::Command::new(cmd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run {cmd}: {e}"))?;
    let start = Instant::now();
    loop {
        if Instant::now() >= deadline {
            child.kill().ok();
            child.wait().ok();
            return Err(deadline_error().message);
        }
        if start.elapsed() >= budget {
            child.kill().ok();
            child.wait().ok();
            return Err(format!("{cmd} timed out"));
        }
        match child
            .try_wait()
            .map_err(|e| format!("waiting for {cmd}: {e}"))?
        {
            Some(status) => {
                let mut out = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    stdout.read_to_string(&mut out).ok();
                }
                if !status.success() {
                    return Err(format!("{cmd} failed"));
                }
                return Ok(out);
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// Resolve `host:port` within the remaining budget: IP literals and
/// `localhost` answer immediately with no subprocess and no DNS, while
/// any other hostname goes through `getent hosts` under the same
/// kill-and-reap bound as [`run_command_with_budget`]. No spawned
/// thread outlives the exchange.
fn resolve_host_with_budget(
    host: &str,
    port: u16,
    deadline: Instant,
    cap: Duration,
) -> Result<std::net::SocketAddr, String> {
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Ok(std::net::SocketAddr::new(ip, port));
    }
    if host.eq_ignore_ascii_case("localhost") {
        let ip: std::net::IpAddr = "127.0.0.1".parse().map_err(|e| format!("localhost: {e}"))?;
        return Ok(std::net::SocketAddr::new(ip, port));
    }
    let budget = remaining(deadline, cap).map_err(|e| e.message)?;
    let out = run_command_with_budget("getent", &["hosts", host], deadline, budget)?;
    for token in out.split_whitespace() {
        if let Ok(ip) = token.parse::<std::net::IpAddr>() {
            return Ok(std::net::SocketAddr::new(ip, port));
        }
    }
    Err(format!("cannot resolve {host}"))
}

/// Attributes requested for each printer: everything the panel shows
/// and nothing more.
const PRINTER_ATTRS: [&str; 16] = [
    "printer-name",
    "printer-uri-supported",
    "device-uri",
    "printer-info",
    "printer-make-and-model",
    "printer-location",
    "printer-state",
    "printer-state-message",
    "printer-state-reasons",
    "printer-is-accepting-jobs",
    "queued-job-count",
    "marker-names",
    "marker-levels",
    "marker-colors",
    "marker-types",
    "printer-commands",
];

/// Attributes requested for each job.
const JOB_ATTRS: [&str; 8] = [
    "job-id",
    "job-uri",
    "job-name",
    "job-originating-user-name",
    "job-state",
    "job-state-message",
    "job-hold-until",
    "job-media-sheets-completed",
];

// ---------------------------------------------------------------------------
// IPP codec
// ---------------------------------------------------------------------------

/// One decoded IPP value. Text-like tags (text, name, keyword, uri,
/// charset, language, MIME, text/name-with-language) all decode to
/// [`IppValue::Text`]; anything unrecognised is kept as bytes so the
/// decoder never loses sync.
#[derive(Debug, Clone, PartialEq)]
pub enum IppValue {
    Integer(i32),
    Boolean(bool),
    Enum(i32),
    Text(String),
    Range(i32, i32),
    Bytes(Vec<u8>),
    Collection(Vec<IppAttr>),
}

/// One IPP attribute: a name, its value tag and one or more values.
#[derive(Debug, Clone, PartialEq)]
pub struct IppAttr {
    pub name: String,
    // Decoded wire fidelity; the model matches on names, never tags.
    #[allow(dead_code)]
    pub tag: u8,
    pub values: Vec<IppValue>,
}

/// A decoded IPP response: the status code plus its attribute groups.
/// Each group is `(group tag, attributes)`; group tags are
/// [`TAG_OPERATION`], [`TAG_JOB`] and [`TAG_PRINTER`].
#[derive(Debug, Clone, PartialEq)]
pub struct IppMessage {
    pub status: u16,
    // Echoed from the request; `exchange` rejects replies whose id
    // does not match the request it sent.
    pub request_id: u32,
    pub groups: Vec<(u8, Vec<IppAttr>)>,
}

impl IppMessage {
    /// All attributes named `name` in groups tagged `group`.
    // Test query helper; production iterates the groups directly.
    #[allow(dead_code)]
    pub fn find<'a>(&'a self, group: u8, name: &str) -> Vec<&'a IppAttr> {
        self.groups
            .iter()
            .filter(|(tag, _)| *tag == group)
            .flat_map(|(_, attrs)| attrs.iter().filter(|a| a.name == name))
            .collect()
    }

    /// First attribute named `name` in any group.
    pub fn find_any(&self, name: &str) -> Option<&IppAttr> {
        self.groups
            .iter()
            .flat_map(|(_, attrs)| attrs.iter())
            .find(|a| a.name == name)
    }

    /// The response's `status-message`, if the server sent one.
    pub fn status_message(&self) -> Option<&str> {
        self.find_any("status-message").and_then(|a| a.first_text())
    }
}

impl IppAttr {
    /// First value as text, if it is text-like.
    pub fn first_text(&self) -> Option<&str> {
        self.values.iter().find_map(|v| match v {
            IppValue::Text(s) => Some(s.as_str()),
            _ => None,
        })
    }

    /// Every text-like value.
    pub fn all_texts(&self) -> Vec<&str> {
        self.values
            .iter()
            .filter_map(|v| match v {
                IppValue::Text(s) => Some(s.as_str()),
                _ => None,
            })
            .collect()
    }

    /// First value as an integer or enum.
    pub fn first_int(&self) -> Option<i32> {
        self.values.iter().find_map(|v| match v {
            IppValue::Integer(n) | IppValue::Enum(n) => Some(*n),
            _ => None,
        })
    }

    /// First value as a boolean.
    pub fn first_bool(&self) -> Option<bool> {
        self.values.iter().find_map(|v| match v {
            IppValue::Boolean(b) => Some(*b),
            _ => None,
        })
    }
}

/// One attribute to encode: `(name, value tag, values)`. Text values
/// are `(tag, bytes)` pairs so one attribute can carry mixed tags.
pub struct OutAttr<'a> {
    pub name: &'a str,
    pub values: Vec<(u8, Vec<u8>)>,
}

impl<'a> OutAttr<'a> {
    pub fn text(name: &'a str, tag: u8, value: &str) -> Self {
        Self {
            name,
            values: vec![(tag, value.as_bytes().to_vec())],
        }
    }

    pub fn texts(name: &'a str, tag: u8, values: &[&str]) -> Self {
        Self {
            name,
            values: values
                .iter()
                .map(|v| (tag, v.as_bytes().to_vec()))
                .collect(),
        }
    }

    pub fn int(name: &'a str, tag: u8, value: i32) -> Self {
        Self {
            name,
            values: vec![(tag, value.to_be_bytes().to_vec())],
        }
    }

    // Encoder completeness; no request sends a boolean today.
    #[allow(dead_code)]
    pub fn boolean(name: &'a str, value: bool) -> Self {
        Self {
            name,
            values: vec![(TAG_BOOLEAN, vec![u8::from(value)])],
        }
    }
}

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_attr(out: &mut Vec<u8>, attr: &OutAttr<'_>) {
    let name = attr.name.as_bytes();
    for (i, (tag, value)) in attr.values.iter().enumerate() {
        out.push(*tag);
        if i == 0 {
            put_u16(out, name.len() as u16);
            out.extend_from_slice(name);
        } else {
            // Additional values repeat the tag with an empty name.
            put_u16(out, 0);
        }
        put_u16(out, value.len() as u16);
        out.extend_from_slice(value);
    }
}

/// Encode an IPP request: header, one attribute block per group tag,
/// then the end-of-attributes tag. A document body, when the operation
/// carries one, is appended by the caller after this encoding.
pub fn encode_request(op: u16, request_id: u32, groups: &[(u8, Vec<OutAttr<'_>>)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(512);
    out.push(IPP_MAJOR);
    out.push(IPP_MINOR);
    put_u16(&mut out, op);
    out.extend_from_slice(&request_id.to_be_bytes());
    for (tag, attrs) in groups {
        out.push(*tag);
        for attr in attrs {
            put_attr(&mut out, attr);
        }
    }
    out.push(TAG_END);
    out
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn rest(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.rest() < n {
            return Err(format!(
                "truncated IPP message at byte {} (need {n})",
                self.pos
            ));
        }
        let out = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn byte(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}

fn decode_value(tag: u8, bytes: &[u8]) -> IppValue {
    match tag {
        TAG_INTEGER if bytes.len() == 4 => {
            IppValue::Integer(i32::from_be_bytes(bytes.try_into().expect("len checked")))
        }
        TAG_ENUM if bytes.len() == 4 => {
            IppValue::Enum(i32::from_be_bytes(bytes.try_into().expect("len checked")))
        }
        TAG_BOOLEAN if bytes.len() == 1 => IppValue::Boolean(bytes[0] != 0),
        TAG_RANGE if bytes.len() == 8 => IppValue::Range(
            i32::from_be_bytes(bytes[0..4].try_into().expect("len checked")),
            i32::from_be_bytes(bytes[4..8].try_into().expect("len checked")),
        ),
        TAG_TEXT | TAG_NAME | TAG_KEYWORD | TAG_URI | TAG_CHARSET | TAG_LANGUAGE | TAG_MIME
        | TAG_OCTET_STRING => IppValue::Text(String::from_utf8_lossy(bytes).into_owned()),
        TAG_TEXT_WITH_LANGUAGE | TAG_NAME_WITH_LANGUAGE => {
            IppValue::Text(String::from_utf8_lossy(text_with_language(bytes)).into_owned())
        }
        _ => IppValue::Bytes(bytes.to_vec()),
    }
}

/// The text part of a text/name-with-language value: 2-byte language
/// length, language, 2-byte text length, text. Malformed values decode
/// whole rather than failing the message.
fn text_with_language(bytes: &[u8]) -> &[u8] {
    if bytes.len() < 2 {
        return bytes;
    }
    let llen = usize::from(u16::from_be_bytes([bytes[0], bytes[1]]));
    let rest = bytes.get(2 + llen..).unwrap_or_default();
    if rest.len() < 2 {
        return bytes;
    }
    let tlen = usize::from(u16::from_be_bytes([rest[0], rest[1]]));
    if rest.len() - 2 < tlen {
        return bytes;
    }
    &rest[2..2 + tlen]
}

/// Decode an IPP response. Collections decode to nested values;
/// unknown tags are kept as bytes; anything truncated or malformed is
/// an error naming the byte offset.
pub fn decode_response(bytes: &[u8]) -> Result<IppMessage, String> {
    let mut cur = Cursor::new(bytes);
    let major = cur.byte()?;
    let _minor = cur.byte()?;
    if major > 2 {
        return Err(format!("unsupported IPP version {major}"));
    }
    let status = cur.u16()?;
    let request_id = cur.u32()?;
    let mut groups: Vec<(u8, Vec<IppAttr>)> = Vec::new();
    let mut current: Option<Vec<IppAttr>> = None;
    let mut current_tag = 0u8;
    loop {
        let tag = cur.byte()?;
        if tag == TAG_END {
            break;
        }
        if tag <= 0x0f {
            // A new group; the previous one (if any) is complete.
            if let Some(attrs) = current.take() {
                groups.push((current_tag, attrs));
            }
            current_tag = tag;
            current = Some(Vec::new());
            continue;
        }
        let attrs = current
            .as_mut()
            .ok_or_else(|| "attribute outside any group".to_string())?;
        let name_len = cur.u16()? as usize;
        let name = if name_len == 0 {
            // An additional value for the previous attribute.
            let prev = attrs
                .last_mut()
                .ok_or_else(|| "additional value without an attribute".to_string())?;
            let value_len = cur.u16()? as usize;
            let raw = cur.take(value_len)?.to_vec();
            if tag == TAG_END_COLLECTION {
                prev.values.push(IppValue::Bytes(raw));
            } else {
                prev.values.push(decode_value(tag, &raw));
            }
            continue;
        } else {
            String::from_utf8_lossy(cur.take(name_len)?).into_owned()
        };
        let value_len = cur.u16()? as usize;
        if tag == TAG_BEGIN_COLLECTION {
            let _ = cur.take(value_len)?;
            let members = decode_collection(&mut cur)?;
            attrs.push(IppAttr {
                name,
                tag,
                values: vec![IppValue::Collection(members)],
            });
            continue;
        }
        let raw = cur.take(value_len)?.to_vec();
        attrs.push(IppAttr {
            name,
            tag,
            values: vec![decode_value(tag, &raw)],
        });
    }
    if let Some(attrs) = current.take() {
        groups.push((current_tag, attrs));
    }
    Ok(IppMessage {
        status,
        request_id,
        groups,
    })
}

/// Decode collection members up to the matching end-of-collection.
/// Member attributes carry a member name, then tagged values.
fn decode_collection(cur: &mut Cursor<'_>) -> Result<Vec<IppAttr>, String> {
    let mut members: Vec<IppAttr> = Vec::new();
    loop {
        let tag = cur.byte()?;
        if tag == TAG_END_COLLECTION {
            let name_len = cur.u16()? as usize;
            let _ = cur.take(name_len)?;
            let value_len = cur.u16()? as usize;
            let _ = cur.take(value_len)?;
            break;
        }
        if tag <= 0x0f {
            return Err("group tag inside collection".to_string());
        }
        let name_len = cur.u16()? as usize;
        let raw_name = cur.take(name_len)?.to_vec();
        let value_len = cur.u16()? as usize;
        let raw = cur.take(value_len)?.to_vec();
        let name = String::from_utf8_lossy(&raw_name).into_owned();
        if name_len == 0 {
            if let Some(prev) = members.last_mut() {
                prev.values.push(decode_value(tag, &raw));
                continue;
            }
        }
        members.push(IppAttr {
            name,
            tag,
            values: vec![decode_value(tag, &raw)],
        });
    }
    Ok(members)
}

// ---------------------------------------------------------------------------
// HTTP framing
// ---------------------------------------------------------------------------

/// A minimal `POST` with `Connection: close`, so the body length is
/// always known: CUPS answers IPP over HTTP/1.1 with Content-Length.
pub fn build_post(resource: &str, body: &[u8]) -> Vec<u8> {
    let head = format!(
        "POST {resource} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut out = head.into_bytes();
    out.extend_from_slice(body);
    out
}

/// Parse a response head: `(status code, head length, content length)`.
/// Interim `1xx` heads are skipped by the caller feeding the remainder.
pub fn parse_response_head(bytes: &[u8]) -> Result<(u16, usize, Option<usize>), String> {
    let end = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "HTTP response without a blank line".to_string())?;
    let head = String::from_utf8_lossy(&bytes[..end]);
    let mut lines = head.lines();
    let status: u16 = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| "HTTP response without a status code".to_string())?;
    let mut length = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                length = value.trim().parse::<usize>().ok();
            }
        }
    }
    Ok((status, end + 4, length))
}

/// Read one HTTP response: head, then exactly Content-Length bytes
/// (or up to [`MAX_BODY`] when the server closes without one).
/// Test-only: production reads under an absolute deadline.
#[cfg(test)]
pub fn read_response(stream: &mut impl Read) -> Result<(u16, Vec<u8>), String> {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    // Read until the head (plus any interim heads) is complete.
    let (status, body_start, length) = loop {
        let n = stream
            .read(&mut chunk)
            .map_err(|e| format!("reading CUPS response: {e}"))?;
        if n == 0 {
            return Err("CUPS closed the connection".to_string());
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_BODY + 8192 {
            return Err("CUPS response head too large".to_string());
        }
        let mut offset = 0;
        let mut found = None;
        while buf[offset..].windows(4).any(|w| w == b"\r\n\r\n") {
            let (status, head_len, length) = parse_response_head(&buf[offset..])?;
            offset += head_len;
            if (100..200).contains(&status) {
                continue; // Interim head (e.g. 100 Continue): skip it.
            }
            found = Some((status, offset, length));
            break;
        }
        if let Some(done) = found {
            break done;
        }
    };
    let mut body = buf[body_start..].to_vec();
    match length {
        Some(n) if n > MAX_BODY => return Err("CUPS response body too large".to_string()),
        Some(n) => {
            while body.len() < n {
                let want = (n - body.len()).min(chunk.len());
                let got = stream
                    .read(&mut chunk[..want])
                    .map_err(|e| format!("reading CUPS response: {e}"))?;
                if got == 0 {
                    return Err("CUPS closed the connection mid-body".to_string());
                }
                body.extend_from_slice(&chunk[..got]);
            }
            body.truncate(n);
        }
        None => loop {
            if body.len() > MAX_BODY {
                return Err("CUPS response body too large".to_string());
            }
            let n = stream
                .read(&mut chunk)
                .map_err(|e| format!("reading CUPS response: {e}"))?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        },
    }
    Ok((status, body))
}

// ---------------------------------------------------------------------------
// Endpoint and errors
// ---------------------------------------------------------------------------

/// How to reach cupsd: its Unix socket or TCP.
#[derive(Debug, Clone, PartialEq)]
pub enum Endpoint {
    Unix(PathBuf),
    Tcp(String, u16),
}

/// Where cupsd should be. `$CUPS_SERVER` wins when set (an absolute
/// path selects the socket, else `host[:port]`); otherwise the known
/// socket paths are probed in order and TCP `localhost:631` is last.
pub fn resolve_endpoints() -> Vec<Endpoint> {
    let server = std::env::var("CUPS_SERVER")
        .ok()
        .filter(|s| !s.trim().is_empty());
    resolve_endpoints_from(server.as_deref())
}

/// [`resolve_endpoints`] with the `$CUPS_SERVER` value passed in.
pub fn resolve_endpoints_from(server: Option<&str>) -> Vec<Endpoint> {
    if let Some(server) = server.map(str::trim).filter(|s| !s.is_empty()) {
        if server.starts_with('/') {
            return vec![Endpoint::Unix(PathBuf::from(server))];
        }
        let (host, port) = match server.rsplit_once(':') {
            Some((h, p)) if !h.is_empty() => (h, p.parse().unwrap_or(CUPS_TCP_PORT)),
            _ => (server, CUPS_TCP_PORT),
        };
        return vec![Endpoint::Tcp(host.to_string(), port)];
    }
    let mut out: Vec<Endpoint> = CUPS_SOCKETS
        .iter()
        .map(|p| Endpoint::Unix(PathBuf::from(p)))
        .collect();
    out.push(Endpoint::Tcp(CUPS_TCP_HOST.to_string(), CUPS_TCP_PORT));
    out
}

/// What went wrong talking to CUPS, in panel terms.
#[derive(Debug, Clone, PartialEq)]
pub enum CupsErrorKind {
    /// No daemon answered anywhere: GNOME's "No Print Service".
    NoPrintService,
    /// cupsd wants credentials (HTTP 401, `not-authenticated`).
    AuthRequired,
    /// cupsd refused (HTTP 403, `forbidden`, `not-authorized`).
    Forbidden,
    /// No such printer or job.
    NotFound,
    /// Another IPP status with the server's message, if any.
    Ipp(u16),
    /// Broken bytes or connection mid-exchange.
    Transport,
}

/// A CUPS failure with a human-readable detail.
#[derive(Debug, Clone, PartialEq)]
pub struct CupsError {
    pub kind: CupsErrorKind,
    pub message: String,
}

impl CupsError {
    fn new(kind: CupsErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// The short label the panel shows for this failure.
    pub fn title(&self) -> &'static str {
        match self.kind {
            CupsErrorKind::NoPrintService => "No Print Service",
            CupsErrorKind::AuthRequired => "Authentication Required",
            CupsErrorKind::Forbidden => "Not Authorized",
            CupsErrorKind::NotFound => "Not Found",
            CupsErrorKind::Ipp(_) | CupsErrorKind::Transport => "Print Error",
        }
    }
}

impl std::fmt::Display for CupsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.title(), self.message)
    }
}

impl std::error::Error for CupsError {}

fn status_error(status: u16, message: &IppMessage) -> CupsError {
    let detail = message.status_message().unwrap_or("").trim().to_string();
    let with_fallback = |fallback: &str| {
        if detail.is_empty() {
            fallback.to_string()
        } else {
            detail.clone()
        }
    };
    match status {
        STATUS_NOT_AUTHENTICATED => CupsError::new(
            CupsErrorKind::AuthRequired,
            with_fallback("the print server wants authentication"),
        ),
        STATUS_FORBIDDEN | STATUS_NOT_AUTHORIZED => CupsError::new(
            CupsErrorKind::Forbidden,
            with_fallback("not allowed to change that printer"),
        ),
        STATUS_NOT_FOUND => CupsError::new(
            CupsErrorKind::NotFound,
            with_fallback("no such printer or job"),
        ),
        other => CupsError::new(
            CupsErrorKind::Ipp(other),
            with_fallback(&format!("print server status {other:#06x}")),
        ),
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// A connected CUPS stream: either transport behind one interface.
enum Stream {
    Tcp(std::net::TcpStream),
    Unix(std::os::unix::net::UnixStream),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Tcp(s) => s.read(buf),
            Stream::Unix(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Tcp(s) => s.write(buf),
            Stream::Unix(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Tcp(s) => s.flush(),
            Stream::Unix(s) => s.flush(),
        }
    }
}

impl Stream {
    fn set_read_timeout(&mut self, timeout: Duration) -> std::io::Result<()> {
        match self {
            Stream::Tcp(s) => s.set_read_timeout(Some(timeout)),
            Stream::Unix(s) => s.set_read_timeout(Some(timeout)),
        }
    }

    fn set_write_timeout(&mut self, timeout: Duration) -> std::io::Result<()> {
        match self {
            Stream::Tcp(s) => s.set_write_timeout(Some(timeout)),
            Stream::Unix(s) => s.set_write_timeout(Some(timeout)),
        }
    }
}

/// Write the whole request, re-arming the socket timeout to the
/// remaining budget before every write: no detached timeout thread.
fn write_all_with_deadline(
    stream: &mut Stream,
    buf: &[u8],
    deadline: Instant,
    io_timeout: Duration,
) -> Result<(), CupsError> {
    let mut written = 0;
    while written < buf.len() {
        let timeout = remaining(deadline, io_timeout)?;
        stream
            .set_write_timeout(timeout)
            .map_err(|e| CupsError::new(CupsErrorKind::Transport, format!("sending: {e}")))?;
        match stream.write(&buf[written..]) {
            Ok(0) => {
                return Err(CupsError::new(
                    CupsErrorKind::Transport,
                    "sending: connection closed",
                ));
            }
            Ok(n) => written += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                if Instant::now() >= deadline {
                    return Err(deadline_error());
                }
                return Err(CupsError::new(
                    CupsErrorKind::Transport,
                    format!("sending: {e}"),
                ));
            }
        }
    }
    let timeout = remaining(deadline, io_timeout)?;
    stream
        .set_write_timeout(timeout)
        .map_err(|e| CupsError::new(CupsErrorKind::Transport, format!("sending: {e}")))?;
    stream.flush().map_err(|e| {
        if Instant::now() >= deadline {
            deadline_error()
        } else {
            CupsError::new(CupsErrorKind::Transport, format!("sending: {e}"))
        }
    })?;
    Ok(())
}

fn read_with_deadline(
    stream: &mut Stream,
    buf: &mut [u8],
    deadline: Instant,
    io_timeout: Duration,
) -> Result<usize, CupsError> {
    let timeout = remaining(deadline, io_timeout)?;
    stream.set_read_timeout(timeout).map_err(|e| {
        CupsError::new(
            CupsErrorKind::Transport,
            format!("reading CUPS response: {e}"),
        )
    })?;
    stream.read(buf).map_err(|e| {
        if Instant::now() >= deadline {
            deadline_error()
        } else {
            CupsError::new(
                CupsErrorKind::Transport,
                format!("reading CUPS response: {e}"),
            )
        }
    })
}

/// Read one HTTP response under an absolute deadline: every read
/// re-arms to the remaining budget, so dribbled bytes fail by the
/// deadline. Same [`MAX_BODY`] cap and interim-`1xx` handling as
/// [`read_response`].
fn read_response_with_deadline(
    stream: &mut Stream,
    deadline: Instant,
    io_timeout: Duration,
) -> Result<(u16, Vec<u8>), CupsError> {
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let (status, body_start, length) = loop {
        if Instant::now() >= deadline {
            return Err(deadline_error());
        }
        let n = read_with_deadline(stream, &mut chunk, deadline, io_timeout)?;
        if n == 0 {
            return Err(CupsError::new(
                CupsErrorKind::Transport,
                "CUPS closed the connection",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_BODY + 8192 {
            return Err(CupsError::new(
                CupsErrorKind::Transport,
                "CUPS response head too large",
            ));
        }
        let mut offset = 0;
        let mut found = None;
        while buf[offset..].windows(4).any(|w| w == b"\r\n\r\n") {
            let (status, head_len, length) = parse_response_head(&buf[offset..])
                .map_err(|e| CupsError::new(CupsErrorKind::Transport, e))?;
            offset += head_len;
            if (100..200).contains(&status) {
                continue; // Interim head (e.g. 100 Continue): skip it.
            }
            found = Some((status, offset, length));
            break;
        }
        if let Some(done) = found {
            break done;
        }
    };
    let mut body = buf[body_start..].to_vec();
    match length {
        Some(n) if n > MAX_BODY => {
            return Err(CupsError::new(
                CupsErrorKind::Transport,
                "CUPS response body too large",
            ));
        }
        Some(n) => {
            while body.len() < n {
                let want = (n - body.len()).min(chunk.len());
                let got = read_with_deadline(stream, &mut chunk[..want], deadline, io_timeout)?;
                if got == 0 {
                    return Err(CupsError::new(
                        CupsErrorKind::Transport,
                        "CUPS closed the connection mid-body",
                    ));
                }
                body.extend_from_slice(&chunk[..got]);
            }
            body.truncate(n);
        }
        None => loop {
            if body.len() > MAX_BODY {
                return Err(CupsError::new(
                    CupsErrorKind::Transport,
                    "CUPS response body too large",
                ));
            }
            let n = read_with_deadline(stream, &mut chunk, deadline, io_timeout)?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        },
    }
    Ok((status, body))
}

/// The user cupsd sees. libcups sends the login name; over the domain
/// socket cupsd authenticates by peer credentials anyway.
pub fn cups_user() -> String {
    cups_user_from(
        std::env::var("USER").ok().as_deref(),
        std::env::var("LOGNAME").ok().as_deref(),
    )
}

/// [`cups_user`] with the environment values passed in.
pub fn cups_user_from(user: Option<&str>, logname: Option<&str>) -> String {
    user.or(logname)
        .filter(|u| !u.trim().is_empty())
        .unwrap_or("anonymous")
        .to_string()
}

/// The `attributes-natural-language` tag from `$LANG` (`en_US.UTF-8`
/// becomes `en-us`), defaulting to `en` like libcups.
pub fn natural_language() -> String {
    language_from(&std::env::var("LANG").unwrap_or_default())
}

/// [`natural_language`] with the `$LANG` value passed in.
pub fn language_from(lang: &str) -> String {
    let base = lang.split(['.', '@']).next().unwrap_or("");
    let tag = base.replace('_', "-");
    if tag.trim().is_empty() {
        "en".to_string()
    } else {
        tag
    }
}

/// A CUPS client: endpoints to probe, the user and language tags,
/// the local `lpoptions` file (if any), a request-id counter and a
/// count of replies discarded for carrying the wrong request id.
/// Cheap to clone for worker threads.
#[derive(Debug)]
pub struct CupsClient {
    endpoints: Vec<Endpoint>,
    user: String,
    lang: String,
    lpoptions_file: Option<PathBuf>,
    next_id: AtomicU32,
    id_mismatches: AtomicU64,
}

impl Clone for CupsClient {
    fn clone(&self) -> Self {
        Self {
            endpoints: self.endpoints.clone(),
            user: self.user.clone(),
            lang: self.lang.clone(),
            lpoptions_file: self.lpoptions_file.clone(),
            next_id: AtomicU32::new(1),
            // Per-client diagnostic: each worker clone counts its own
            // exchanges, and surfaces mismatches as errors.
            id_mismatches: AtomicU64::new(0),
        }
    }
}

impl CupsClient {
    /// The system daemon: resolved endpoints, the login user, the
    /// user's local `lpoptions` file when `$HOME` names one.
    pub fn system() -> Self {
        Self {
            endpoints: resolve_endpoints(),
            user: cups_user(),
            lang: natural_language(),
            lpoptions_file: lpoptions_file_from_home(std::env::var("HOME").ok().as_deref()),
            next_id: AtomicU32::new(1),
            id_mismatches: AtomicU64::new(0),
        }
    }

    /// One explicit endpoint (tests and `$CUPS_SERVER` overrides).
    /// No local `lpoptions` file: tests that need one pass it with
    /// [`CupsClient::with_lpoptions_file`] instead of mutating `HOME`.
    // Test constructor; production resolves the system daemon.
    #[allow(dead_code)]
    pub fn single(endpoint: Endpoint) -> Self {
        Self {
            endpoints: vec![endpoint],
            user: cups_user(),
            lang: natural_language(),
            lpoptions_file: None,
            next_id: AtomicU32::new(1),
            id_mismatches: AtomicU64::new(0),
        }
    }

    /// Use `path` as the local `lpoptions` file (`None` for no local
    /// file: the default comes from the server). Test-only
    /// indirection so no test mutates the process-global `HOME`.
    #[cfg(test)]
    pub fn with_lpoptions_file(mut self, path: Option<PathBuf>) -> Self {
        self.lpoptions_file = path;
        self
    }

    fn request_id(&self) -> u32 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Replies discarded for a request-id mismatch on this client.
    // Test probe; production surfaces each mismatch as an error.
    #[allow(dead_code)]
    pub fn id_mismatch_count(&self) -> u64 {
        self.id_mismatches.load(Ordering::Relaxed)
    }

    /// The operation attributes every request carries: charset,
    /// language and user, exactly as libcups sends them.
    fn op_base(&self) -> Vec<OutAttr<'_>> {
        vec![
            OutAttr::text("attributes-charset", TAG_CHARSET, "utf-8"),
            OutAttr::text("attributes-natural-language", TAG_LANGUAGE, &self.lang),
            OutAttr::text("requesting-user-name", TAG_NAME, &self.user),
        ]
    }

    fn connect_with_budget(
        &self,
        deadline: Instant,
        connect_timeout: Duration,
        io_timeout: Duration,
    ) -> Result<Stream, CupsError> {
        let mut detail = String::new();
        for endpoint in &self.endpoints {
            if Instant::now() >= deadline {
                return Err(deadline_error());
            }
            let attempt: Result<Stream, String> = match endpoint {
                Endpoint::Unix(path) => {
                    let budget = remaining(deadline, connect_timeout)?;
                    unix_connect_with_timeout(path, budget)
                        .map_err(|e| e.to_string())
                        .and_then(|s| {
                            let budget = remaining(deadline, io_timeout).map_err(|e| e.message)?;
                            s.set_read_timeout(Some(budget))
                                .and_then(|()| s.set_write_timeout(Some(budget)))
                                .map_err(|e| e.to_string())?;
                            Ok(Stream::Unix(s))
                        })
                }
                Endpoint::Tcp(host, port) => {
                    // Resolution first under the remaining budget (no
                    // detached thread), then the connect under what is
                    // left: resolution time counts against the exchange.
                    match resolve_host_with_budget(host, *port, deadline, connect_timeout) {
                        Err(e) => Err(e),
                        Ok(addr) => {
                            let budget = remaining(deadline, connect_timeout)?;
                            std::net::TcpStream::connect_timeout(&addr, budget)
                                .map_err(|e| e.to_string())
                                .and_then(|s| {
                                    let budget =
                                        remaining(deadline, io_timeout).map_err(|e| e.message)?;
                                    s.set_read_timeout(Some(budget))
                                        .and_then(|()| s.set_write_timeout(Some(budget)))
                                        .map_err(|e| e.to_string())?;
                                    Ok(Stream::Tcp(s))
                                })
                        }
                    }
                }
            };
            match attempt {
                Ok(stream) => return Ok(stream),
                Err(e) => {
                    if Instant::now() >= deadline {
                        return Err(deadline_error());
                    }
                    if !detail.is_empty() {
                        detail.push_str("; ");
                    }
                    detail.push_str(&format!("{endpoint:?}: {e}"));
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(deadline_error());
        }
        Err(CupsError::new(
            CupsErrorKind::NoPrintService,
            format!("no print server answered ({detail})"),
        ))
    }

    /// One IPP exchange: encode `groups` (operation group first),
    /// POST to `/` like libcups does, read and decode the response,
    /// and map failures to [`CupsError`]. `document`, when present, is
    /// appended after the encoding. A reply whose request id is not the
    /// one just sent is discarded and counted, never applied. The whole
    /// exchange must finish by [`EXCHANGE_TIMEOUT`]: every
    /// connect/read/write re-arms to the remaining budget.
    pub fn exchange(
        &self,
        op: u16,
        groups: Vec<(u8, Vec<OutAttr<'_>>)>,
        document: Option<&[u8]>,
    ) -> Result<IppMessage, CupsError> {
        self.exchange_with_budget(
            op,
            groups,
            document,
            Instant::now() + EXCHANGE_TIMEOUT,
            CONNECT_TIMEOUT,
            IO_TIMEOUT,
        )
    }

    fn exchange_with_budget(
        &self,
        op: u16,
        groups: Vec<(u8, Vec<OutAttr<'_>>)>,
        document: Option<&[u8]>,
        deadline: Instant,
        connect_timeout: Duration,
        io_timeout: Duration,
    ) -> Result<IppMessage, CupsError> {
        if Instant::now() >= deadline {
            return Err(deadline_error());
        }
        let sent_id = self.request_id();
        let mut body = encode_request(op, sent_id, &groups);
        if let Some(doc) = document {
            body.extend_from_slice(doc);
        }
        let request = build_post("/", &body);
        let mut stream = self.connect_with_budget(deadline, connect_timeout, io_timeout)?;
        write_all_with_deadline(&mut stream, &request, deadline, io_timeout)?;
        let (http, reply) = read_response_with_deadline(&mut stream, deadline, io_timeout)?;
        match http {
            401 => {
                return Err(CupsError::new(
                    CupsErrorKind::AuthRequired,
                    "the print server wants authentication",
                ));
            }
            403 => {
                return Err(CupsError::new(
                    CupsErrorKind::Forbidden,
                    "not allowed to talk to the print server",
                ));
            }
            s if !(200..300).contains(&s) => {
                return Err(CupsError::new(
                    CupsErrorKind::Transport,
                    format!("print server HTTP {s}"),
                ));
            }
            _ => {}
        }
        let message =
            decode_response(&reply).map_err(|e| CupsError::new(CupsErrorKind::Transport, e))?;
        if message.request_id != sent_id {
            self.id_mismatches.fetch_add(1, Ordering::Relaxed);
            let got = message.request_id;
            return Err(CupsError::new(
                CupsErrorKind::Transport,
                format!("print server mixed up replies (request {sent_id}, reply {got})"),
            ));
        }
        if message.status != STATUS_OK {
            return Err(status_error(message.status, &message));
        }
        Ok(message)
    }

    /// The printers the server knows, in server order.
    /// Test-only: production lists via the snapshot budget.
    #[cfg(test)]
    pub fn printers(&self) -> Result<Vec<Printer>, CupsError> {
        self.printers_with_budget(
            Instant::now() + EXCHANGE_TIMEOUT,
            CONNECT_TIMEOUT,
            IO_TIMEOUT,
            EXCHANGE_TIMEOUT,
        )
    }

    fn printers_with_budget(
        &self,
        outer: Instant,
        connect_timeout: Duration,
        io_timeout: Duration,
        exchange_timeout: Duration,
    ) -> Result<Vec<Printer>, CupsError> {
        let mut op = self.op_base();
        op.push(OutAttr::texts(
            "requested-attributes",
            TAG_KEYWORD,
            &PRINTER_ATTRS,
        ));
        let deadline = effective_exchange_deadline(outer, exchange_timeout);
        let reply = self.exchange_with_budget(
            OP_CUPS_GET_PRINTERS,
            vec![(TAG_OPERATION, op)],
            None,
            deadline,
            connect_timeout,
            io_timeout,
        )?;
        let mut out = Vec::new();
        for (tag, attrs) in &reply.groups {
            if *tag != TAG_PRINTER {
                continue;
            }
            if let Some(printer) = Printer::from_attrs(attrs) {
                out.push(printer);
            }
        }
        Ok(out)
    }

    fn default_name_with_budget(
        &self,
        outer: Instant,
        connect_timeout: Duration,
        io_timeout: Duration,
        exchange_timeout: Duration,
    ) -> Option<String> {
        if let Some(path) = self.lpoptions_file.as_ref() {
            if let Some(text) = read_lpoptions_bounded(path) {
                if let Some(name) = default_from_lpoptions(&text) {
                    return Some(name);
                }
            }
        }
        self.server_default_name_with_budget(outer, connect_timeout, io_timeout, exchange_timeout)
    }

    /// The server-wide default via `CUPS-Get-Default`.
    /// Test-only: production resolves via the snapshot budget.
    #[cfg(test)]
    pub fn server_default_name(&self) -> Option<String> {
        self.server_default_name_with_budget(
            Instant::now() + EXCHANGE_TIMEOUT,
            CONNECT_TIMEOUT,
            IO_TIMEOUT,
            EXCHANGE_TIMEOUT,
        )
    }

    fn server_default_name_with_budget(
        &self,
        outer: Instant,
        connect_timeout: Duration,
        io_timeout: Duration,
        exchange_timeout: Duration,
    ) -> Option<String> {
        let op = self.op_base();
        let deadline = effective_exchange_deadline(outer, exchange_timeout);
        let reply = self
            .exchange_with_budget(
                OP_CUPS_GET_DEFAULT,
                vec![(TAG_OPERATION, op)],
                None,
                deadline,
                connect_timeout,
                io_timeout,
            )
            .ok()?;
        reply
            .find_any("printer-name")
            .and_then(|a| a.first_text())
            .map(str::to_string)
    }

    /// Fresh attributes for one printer, e.g. before an action.
    // Reserved client query; actions use the listing's fresh state.
    #[allow(dead_code)]
    pub fn printer_details(&self, name: &str) -> Result<Printer, CupsError> {
        let mut op = self.op_base();
        op.push(OutAttr::text("printer-uri", TAG_URI, &printer_uri(name)));
        op.push(OutAttr::texts(
            "requested-attributes",
            TAG_KEYWORD,
            &PRINTER_ATTRS,
        ));
        let reply = self.exchange(OP_GET_PRINTER_ATTRIBUTES, vec![(TAG_OPERATION, op)], None)?;
        reply
            .groups
            .iter()
            .filter(|(tag, _)| *tag == TAG_PRINTER)
            .find_map(|(_, attrs)| Printer::from_attrs(attrs))
            .ok_or_else(|| {
                CupsError::new(CupsErrorKind::NotFound, format!("no such printer: {name}"))
            })
    }

    /// Active (`not-completed`) jobs on one printer, newest last like
    /// the jobs dialog lists them. `my_only` restricts to our user.
    /// Test-only: production lists via the snapshot/action budget.
    #[cfg(test)]
    pub fn jobs(&self, printer_uri: &str, my_only: bool) -> Result<Vec<PrintJob>, CupsError> {
        self.jobs_with_budget(
            printer_uri,
            my_only,
            Instant::now() + EXCHANGE_TIMEOUT,
            CONNECT_TIMEOUT,
            IO_TIMEOUT,
            EXCHANGE_TIMEOUT,
        )
    }

    fn jobs_with_budget(
        &self,
        printer_uri: &str,
        my_only: bool,
        outer: Instant,
        connect_timeout: Duration,
        io_timeout: Duration,
        exchange_timeout: Duration,
    ) -> Result<Vec<PrintJob>, CupsError> {
        let mut op = self.op_base();
        op.push(OutAttr::text("printer-uri", TAG_URI, printer_uri));
        if my_only {
            op.push(OutAttr::text("requesting-user-name", TAG_NAME, &self.user));
        }
        op.push(OutAttr::text("which-jobs", TAG_KEYWORD, "not-completed"));
        op.push(OutAttr::texts(
            "requested-attributes",
            TAG_KEYWORD,
            &JOB_ATTRS,
        ));
        let deadline = effective_exchange_deadline(outer, exchange_timeout);
        let reply = self.exchange_with_budget(
            OP_GET_JOBS,
            vec![(TAG_OPERATION, op)],
            None,
            deadline,
            connect_timeout,
            io_timeout,
        )?;
        let mut out = Vec::new();
        for (tag, attrs) in &reply.groups {
            if *tag != TAG_JOB {
                continue;
            }
            if let Some(job) = PrintJob::from_attrs(attrs) {
                out.push(job);
            }
        }
        Ok(out)
    }

    /// Cancel one job by id.
    pub fn cancel_job(&self, printer_uri: &str, job_id: i32) -> Result<(), CupsError> {
        self.cancel_job_with_budget(
            printer_uri,
            job_id,
            Instant::now() + EXCHANGE_TIMEOUT,
            CONNECT_TIMEOUT,
            IO_TIMEOUT,
            EXCHANGE_TIMEOUT,
        )
    }

    fn cancel_job_with_budget(
        &self,
        printer_uri: &str,
        job_id: i32,
        outer: Instant,
        connect_timeout: Duration,
        io_timeout: Duration,
        exchange_timeout: Duration,
    ) -> Result<(), CupsError> {
        let mut op = self.op_base();
        op.push(OutAttr::text("printer-uri", TAG_URI, printer_uri));
        op.push(OutAttr::int("job-id", TAG_INTEGER, job_id));
        let deadline = effective_exchange_deadline(outer, exchange_timeout);
        self.exchange_with_budget(
            OP_CANCEL_JOB,
            vec![(TAG_OPERATION, op)],
            None,
            deadline,
            connect_timeout,
            io_timeout,
        )?;
        Ok(())
    }

    /// Cancel our active jobs on one printer (the dialog's Clear All).
    /// Returns how many were cancelled; stops at the first failure.
    /// The list plus the serial cancels share [`ACTION_TIMEOUT`].
    pub fn clear_my_jobs(&self, printer_uri: &str) -> Result<usize, CupsError> {
        self.clear_my_jobs_with_budget(
            printer_uri,
            Instant::now() + ACTION_TIMEOUT,
            CONNECT_TIMEOUT,
            IO_TIMEOUT,
            EXCHANGE_TIMEOUT,
        )
    }

    fn clear_my_jobs_with_budget(
        &self,
        printer_uri: &str,
        outer: Instant,
        connect_timeout: Duration,
        io_timeout: Duration,
        exchange_timeout: Duration,
    ) -> Result<usize, CupsError> {
        let jobs = self.jobs_with_budget(
            printer_uri,
            true,
            outer,
            connect_timeout,
            io_timeout,
            exchange_timeout,
        )?;
        let mut done = 0;
        for job in &jobs {
            if !job.state.is_active() {
                continue;
            }
            self.cancel_job_with_budget(
                printer_uri,
                job.id,
                outer,
                connect_timeout,
                io_timeout,
                exchange_timeout,
            )?;
            done += 1;
        }
        Ok(done)
    }

    /// Submit `document` to one printer as `job_name`.
    pub fn print_data(
        &self,
        printer_uri: &str,
        document: &[u8],
        format: &str,
        job_name: &str,
    ) -> Result<i32, CupsError> {
        let mut op = self.op_base();
        op.push(OutAttr::text("printer-uri", TAG_URI, printer_uri));
        op.push(OutAttr::text("job-name", TAG_NAME, job_name));
        let job = vec![OutAttr::text("document-format", TAG_MIME, format)];
        let reply = self.exchange(
            OP_PRINT_JOB,
            vec![(TAG_OPERATION, op), (TAG_JOB, job)],
            Some(document),
        )?;
        reply
            .find_any("job-id")
            .and_then(|a| a.first_int())
            .ok_or_else(|| {
                CupsError::new(
                    CupsErrorKind::Transport,
                    "print server did not return a job id",
                )
            })
    }

    /// Run a maintenance command (`Clean`, `PrintSelfTestPage`) as a
    /// command job, exactly as `pp-maintenance-command.c` does.
    pub fn maintenance(
        &self,
        printer_uri: &str,
        command: &str,
        title: &str,
    ) -> Result<i32, CupsError> {
        self.print_data(printer_uri, command.as_bytes(), FORMAT_CUPS_COMMAND, title)
    }

    /// Whether the printer advertises a maintenance `command` in its
    /// `printer-commands` attribute.
    // Reserved probe; the panel reads the listing's commands directly.
    #[allow(dead_code)]
    pub fn supports_command(&self, name: &str, command: &str) -> bool {
        self.printer_details(name)
            .map(|p| p.commands.iter().any(|c| c == command))
            .unwrap_or(false)
    }

    /// Print a test page: CUPS's own `testprint` file when installed,
    /// else the printer's self-test maintenance command. This is
    /// `test_page_cb` in `pp-options-dialog.c`.
    pub fn print_test_page(&self, printer_uri: &str) -> Result<i32, CupsError> {
        if let Some(path) = find_testprint() {
            let data = std::fs::read(&path).map_err(|e| {
                CupsError::new(
                    CupsErrorKind::Transport,
                    format!("reading {}: {e}", path.display()),
                )
            })?;
            return self.print_data(printer_uri, &data, FORMAT_AUTO, "Test Page");
        }
        self.maintenance(printer_uri, "PrintSelfTestPage", "Test page")
    }

    /// Make `name` the default printer. Needs cupsd admin rights, so a
    /// denied attempt surfaces as [`CupsErrorKind::AuthRequired`] or
    /// [`CupsErrorKind::Forbidden`]: GNOME routes this through
    /// cups-pk-helper instead (see the module docs).
    pub fn set_default(&self, name: &str) -> Result<(), CupsError> {
        let mut op = self.op_base();
        op.push(OutAttr::text("printer-uri", TAG_URI, &printer_uri(name)));
        self.exchange(OP_CUPS_SET_DEFAULT, vec![(TAG_OPERATION, op)], None)?;
        Ok(())
    }

    /// Pause or resume (GNOME's Restart) `name`. Same privilege caveat
    /// as [`CupsClient::set_default`].
    pub fn set_enabled(&self, name: &str, enabled: bool) -> Result<(), CupsError> {
        let mut op = self.op_base();
        op.push(OutAttr::text("printer-uri", TAG_URI, &printer_uri(name)));
        let code = if enabled {
            OP_RESUME_PRINTER
        } else {
            OP_PAUSE_PRINTER
        };
        self.exchange(code, vec![(TAG_OPERATION, op)], None)?;
        Ok(())
    }

    /// Delete `name`. Same privilege caveat as
    /// [`CupsClient::set_default`].
    pub fn remove_printer(&self, name: &str) -> Result<(), CupsError> {
        let mut op = self.op_base();
        op.push(OutAttr::text("printer-uri", TAG_URI, &printer_uri(name)));
        self.exchange(OP_CUPS_DELETE_PRINTER, vec![(TAG_OPERATION, op)], None)?;
        Ok(())
    }

    /// Everything the panel renders: printers with the default marked,
    /// plus each printer's active jobs. The listing, the default lookup
    /// and the serial per-printer job queries share
    /// [`SNAPSHOT_TIMEOUT`]: each exchange re-arms to the remaining
    /// budget, so two dribbling peers cannot hold a worker forever.
    pub fn snapshot(&self) -> Result<Snapshot, CupsError> {
        self.snapshot_with_budget(
            Instant::now() + SNAPSHOT_TIMEOUT,
            CONNECT_TIMEOUT,
            IO_TIMEOUT,
            EXCHANGE_TIMEOUT,
        )
    }

    fn snapshot_with_budget(
        &self,
        outer: Instant,
        connect_timeout: Duration,
        io_timeout: Duration,
        exchange_timeout: Duration,
    ) -> Result<Snapshot, CupsError> {
        let mut printers =
            self.printers_with_budget(outer, connect_timeout, io_timeout, exchange_timeout)?;
        let default =
            self.default_name_with_budget(outer, connect_timeout, io_timeout, exchange_timeout);
        if let Some(name) = &default {
            for p in &mut printers {
                p.is_default = p.name == *name;
            }
        }
        let mut entries = Vec::with_capacity(printers.len());
        for printer in printers {
            // Jobs failing must not hide the printer itself.
            let jobs = self
                .jobs_with_budget(
                    &printer.uri,
                    false,
                    outer,
                    connect_timeout,
                    io_timeout,
                    exchange_timeout,
                )
                .unwrap_or_default();
            entries.push(PrinterEntry { printer, jobs });
        }
        Ok(Snapshot { entries })
    }
}

/// The `printer-uri` libcups addresses: `ipp://localhost/printers/…`.
pub fn printer_uri(name: &str) -> String {
    format!("ipp://localhost/printers/{}", uri_escape(name))
}

/// Percent-encode a URI path segment (RFC 3986 unreserved set kept).
pub fn uri_escape(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for b in segment.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The `Default` line of `~/.cups/lpoptions`, if any.
pub fn default_from_lpoptions(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        match (words.next(), words.next()) {
            (Some("Default"), Some(name)) => Some(name.to_string()),
            _ => None,
        }
    })
}

/// The local `lpoptions` file for one `$HOME` value, if any. `None`
/// means no local file: the default comes from the server. Tests pass
/// their own path (or `None`) through the client instead of mutating
/// the process-global `HOME`.
pub fn lpoptions_file_from_home(home: Option<&str>) -> Option<PathBuf> {
    home.filter(|h| !h.trim().is_empty())
        .map(|h| PathBuf::from(h).join(".cups/lpoptions"))
}

/// Read one `lpoptions` file with no blocking and no race: open first
/// (nonblocking, so a FIFO open never waits for a writer), then `fstat`
/// the descriptor and refuse anything that is not a regular file or is
/// over [`LPOPTIONS_MAX`], then read at most the cap plus one byte (so
/// a file that grew after the `fstat` is still refused). Never
/// stat-then-open by path, and no detached timeout thread: a FIFO or a
/// huge file falls through to the server default immediately.
fn read_lpoptions_bounded(path: &Path) -> Option<String> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    if meta.len() > LPOPTIONS_MAX as u64 {
        return None;
    }
    let mut text = String::new();
    file.take((LPOPTIONS_MAX + 1) as u64)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() > LPOPTIONS_MAX {
        return None;
    }
    Some(text)
}

/// Where CUPS's test-page file lives. `$CUPS_DATADIR` wins (as in
/// `test_page_cb`); otherwise the two installed data directories.
pub fn testprint_candidates(datadir: Option<&str>) -> Vec<PathBuf> {
    let roots: Vec<&str> = match datadir {
        Some(dir) if !dir.is_empty() => vec![dir],
        _ => vec!["/usr/share/cups", "/usr/local/share/cups"],
    };
    let mut out = Vec::new();
    for root in roots {
        for leaf in ["data/testprint", "data/testprint.ps"] {
            out.push(PathBuf::from(root).join(leaf));
        }
    }
    out
}

/// The installed test-page file, if it is readable.
pub fn find_testprint() -> Option<PathBuf> {
    let datadir = std::env::var("CUPS_DATADIR").ok().filter(|d| !d.is_empty());
    testprint_candidates(datadir.as_deref())
        .into_iter()
        .find(|p| std::fs::File::open(p).is_ok())
}

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// A printer's state: idle (ready), processing or stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrinterState {
    Ready,
    Processing,
    Stopped,
}

impl PrinterState {
    pub fn from_int(v: i32) -> Self {
        match v {
            PSTATE_IDLE => Self::Ready,
            PSTATE_PROCESSING => Self::Processing,
            PSTATE_STOPPED => Self::Stopped,
            _ => Self::Ready,
        }
    }

    /// The Status row, exactly as `pp_printer_entry_update` labels it.
    pub fn label(self, accepting_jobs: bool) -> &'static str {
        match (self, accepting_jobs) {
            (Self::Ready, true) => "Ready",
            (Self::Ready, false) => "Does not accept jobs",
            (Self::Processing, _) => "Processing",
            (Self::Stopped, _) => "Stopped",
        }
    }
}

/// A job's state. Active jobs are pending, held, processing or stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Pending,
    Held,
    Processing,
    Stopped,
    Canceled,
    Aborted,
    Completed,
    Unknown(i32),
}

impl JobState {
    pub fn from_int(v: i32) -> Self {
        match v {
            JSTATE_PENDING => Self::Pending,
            4 => Self::Held,
            5 => Self::Processing,
            6 => Self::Stopped,
            7 => Self::Canceled,
            8 => Self::Aborted,
            JSTATE_COMPLETED => Self::Completed,
            other => Self::Unknown(other),
        }
    }

    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Pending | Self::Held | Self::Processing | Self::Stopped
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Held => "Held",
            Self::Processing => "Printing",
            Self::Stopped => "Stopped",
            Self::Canceled => "Canceled",
            Self::Aborted => "Aborted",
            Self::Completed => "Completed",
            Self::Unknown(_) => "Unknown",
        }
    }
}

/// `printer-state-reasons` keywords and the status each one shows.
/// Verbatim from `pp_printer_entry_update` (untranslated sources).
const REASON_STATUS: [(&str, &str); 16] = [
    ("toner-low", "Low on toner"),
    ("toner-empty", "Out of toner"),
    ("developer-low", "Low on developer"),
    ("developer-empty", "Out of developer"),
    ("marker-supply-low", "Low on a marker supply"),
    ("marker-supply-empty", "Out of a marker supply"),
    ("cover-open", "Open cover"),
    ("door-open", "Open door"),
    ("media-low", "Low on paper"),
    ("media-empty", "Out of paper"),
    ("offline", "Offline"),
    ("paused", "Stopped"),
    ("marker-waste-almost-full", "Waste receptacle almost full"),
    ("marker-waste-full", "Waste receptacle full"),
    (
        "opc-near-eol",
        "The optical photo conductor is near end of life",
    ),
    (
        "opc-life-over",
        "The optical photo conductor is no longer functioning",
    ),
];

/// The status line for comma-joined `printer-state-reasons`: the first
/// error, else the first warning, else the first report. A reason
/// ending in `-report` is a report, in `-warning` a warning, and
/// anything else (bare or `-error`) an error. `none` means no status.
/// This is the severity loop in `pp_printer_entry_update`.
pub fn reason_status(reasons: &str) -> Option<&str> {
    if reasons.trim().is_empty() || reasons.trim() == "none" {
        return None;
    }
    let mut error = None;
    let mut warning = None;
    let mut report = None;
    for reason in reasons.split(',') {
        let reason = reason.trim();
        let Some(index) = REASON_STATUS
            .iter()
            .position(|(key, _)| reason.starts_with(key))
        else {
            continue;
        };
        if reason.ends_with("-report") {
            report.get_or_insert(index);
        } else if reason.ends_with("-warning") {
            warning.get_or_insert(index);
        } else {
            error.get_or_insert(index);
        }
    }
    error.or(warning).or(report).map(|i| REASON_STATUS[i].1)
}

/// The error banner under a stopped (or non-accepting) printer: shown
/// exactly when GNOME shows it.
pub fn error_banner(
    state: PrinterState,
    accepting_jobs: bool,
    status: Option<&str>,
) -> Option<&str> {
    match (state, accepting_jobs, status) {
        (PrinterState::Stopped, _, Some(s)) if !s.is_empty() => Some(s),
        (_, false, Some(s)) if !s.is_empty() => Some(s),
        _ => None,
    }
}

/// Whether GNOME's details dialog accepts `name`: printer names cannot
/// contain SPACE, TAB, `#` or `/`.
// The tested rename rule; the shell panel has no rename UI.
#[allow(dead_code)]
pub fn valid_printer_name(name: &str) -> bool {
    !name.is_empty() && !name.chars().any(|c| matches!(c, ' ' | '\t' | '#' | '/'))
}

/// One ink or toner marker: a name (`marker-names`), a level in
/// percent (`marker-levels`, negative when the printer does not know),
/// and the color and type strings when reported.
#[derive(Debug, Clone, PartialEq)]
pub struct Marker {
    pub name: String,
    pub level: i32,
    // Decoded for model fidelity; the Ink Level row shows name and level.
    #[allow(dead_code)]
    pub color: String,
    // Decoded for model fidelity; the Ink Level row shows name and level.
    #[allow(dead_code)]
    pub kind: String,
}

/// A printer as the panel shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Printer {
    pub name: String,
    pub uri: String,
    pub device_uri: String,
    // CUPS's description string; the panel renders GNOME's rows.
    #[allow(dead_code)]
    pub info: String,
    pub make_and_model: String,
    pub location: String,
    pub state: PrinterState,
    // The server's detail; the Status row derives from state and reasons.
    #[allow(dead_code)]
    pub state_message: String,
    pub reasons: Vec<String>,
    pub accepting_jobs: bool,
    pub is_default: bool,
    // Queued count from the listing; the panel lists the jobs themselves.
    #[allow(dead_code)]
    pub queued: i32,
    pub commands: Vec<String>,
    pub markers: Vec<Marker>,
}

impl Printer {
    /// Build from one printer-attributes group. `None` without a
    /// `printer-name`: nameless groups are not printers.
    pub fn from_attrs(attrs: &[IppAttr]) -> Option<Self> {
        let get = |name: &str| attrs.iter().find(|a| a.name == name);
        let text = |name: &str| {
            get(name)
                .and_then(|a| a.first_text())
                .unwrap_or("")
                .to_string()
        };
        let name = get("printer-name")?.first_text()?.to_string();
        let uri = {
            let reported = text("printer-uri-supported");
            if reported.is_empty() {
                printer_uri(&name)
            } else {
                reported
            }
        };
        let reasons: Vec<String> = get("printer-state-reasons")
            .map(|a| a.all_texts().iter().map(|s| s.to_string()).collect())
            .unwrap_or_default();
        let split = |name: &str| -> Vec<String> {
            // libcups joins multi-valued options with commas; CUPS may
            // also send one value per attribute entry. Accept both.
            get(name)
                .map(|a| {
                    a.values
                        .iter()
                        .filter_map(|v| match v {
                            IppValue::Text(s) => Some(s.clone()),
                            IppValue::Integer(n) => Some(n.to_string()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .collect()
                })
                .unwrap_or_default()
        };
        let names = split("marker-names");
        let levels = split("marker-levels");
        let colors = split("marker-colors");
        let kinds = split("marker-types");
        let markers = names
            .into_iter()
            .enumerate()
            .map(|(i, name)| Marker {
                name,
                level: levels.get(i).and_then(|l| l.parse().ok()).unwrap_or(-1),
                color: colors.get(i).cloned().unwrap_or_default(),
                kind: kinds.get(i).cloned().unwrap_or_default(),
            })
            .collect();
        Some(Self {
            name,
            uri,
            device_uri: text("device-uri"),
            info: text("printer-info"),
            make_and_model: text("printer-make-and-model"),
            location: text("printer-location"),
            state: PrinterState::from_int(
                get("printer-state")
                    .and_then(|a| a.first_int())
                    .unwrap_or(3),
            ),
            state_message: text("printer-state-message"),
            reasons,
            accepting_jobs: get("printer-is-accepting-jobs")
                .and_then(|a| a.first_bool())
                .unwrap_or(true),
            is_default: false,
            queued: get("queued-job-count")
                .and_then(|a| a.first_int())
                .unwrap_or(0),
            commands: get("printer-commands")
                .map(|a| a.all_texts().iter().map(|s| s.to_string()).collect())
                .unwrap_or_default(),
            markers,
        })
    }

    /// The Status row plus the optional error banner.
    pub fn status(&self) -> (String, Option<String>) {
        let joined = self.reasons.join(",");
        let reason = reason_status(&joined);
        let banner = error_banner(self.state, self.accepting_jobs, reason).map(str::to_string);
        (self.state.label(self.accepting_jobs).to_string(), banner)
    }

    /// The Ink Level row, or `None` when the printer reports nothing.
    /// Unknown levels (`marker-levels` negative) show the name alone.
    pub fn ink_summary(&self) -> Option<String> {
        if self.markers.is_empty() {
            return None;
        }
        let parts: Vec<String> = self
            .markers
            .iter()
            .map(|m| {
                if m.level < 0 {
                    m.name.clone()
                } else {
                    format!("{} {}%", m.name, m.level.clamp(0, 100))
                }
            })
            .collect();
        Some(parts.join(", "))
    }
}

/// One print job as the jobs list shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct PrintJob {
    pub id: i32,
    // The job's URI; actions address the printer URI plus the job id.
    #[allow(dead_code)]
    pub uri: String,
    pub name: String,
    pub user: String,
    pub state: JobState,
    pub message: String,
    pub hold_until: Option<String>,
    // Completed sheet count; the jobs list shows state, not progress.
    #[allow(dead_code)]
    pub sheets_completed: i32,
}

impl PrintJob {
    /// Build from one job-attributes group. `None` without a `job-id`.
    pub fn from_attrs(attrs: &[IppAttr]) -> Option<Self> {
        let get = |name: &str| attrs.iter().find(|a| a.name == name);
        let text = |name: &str| {
            get(name)
                .and_then(|a| a.first_text())
                .unwrap_or("")
                .to_string()
        };
        Some(Self {
            id: get("job-id")?.first_int()?,
            uri: text("job-uri"),
            name: {
                let name = text("job-name");
                if name.is_empty() {
                    "Untitled".to_string()
                } else {
                    name
                }
            },
            user: text("job-originating-user-name"),
            state: JobState::from_int(get("job-state").and_then(|a| a.first_int()).unwrap_or(3)),
            message: text("job-state-message"),
            hold_until: get("job-hold-until")
                .and_then(|a| a.first_text())
                .map(str::to_string),
            sheets_completed: get("job-media-sheets-completed")
                .and_then(|a| a.first_int())
                .unwrap_or(0),
        })
    }

    /// Whether the job waits on the Authenticate flow: held with
    /// `job-hold-until` of `auth-info-required`, as `pp-printer.c`
    /// detects it.
    pub fn auth_required(&self) -> bool {
        self.state == JobState::Held && self.hold_until.as_deref() == Some("auth-info-required")
    }
}

/// One panel entry: a printer and its active jobs.
#[derive(Debug, Clone, PartialEq)]
pub struct PrinterEntry {
    pub printer: Printer,
    pub jobs: Vec<PrintJob>,
}

/// Everything the panel renders in one refresh.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub entries: Vec<PrinterEntry>,
}

// ---------------------------------------------------------------------------
// Panel
// ---------------------------------------------------------------------------

/// Open GNOME Settings on its printers panel, as the shell's other
/// "... Settings" rows open theirs. Full add-printer discovery lives
/// there until this panel grows it.
pub fn open_printers_settings() {
    let _ = std::process::Command::new("gnome-control-center")
        .arg("printers")
        .spawn();
}

/// One privileged-or-slow action, run on a worker thread.
#[derive(Debug, Clone)]
enum Action {
    TestPage { uri: String },
    Clean { uri: String },
    SetDefault { name: String },
    SetEnabled { name: String, enabled: bool },
    Remove { name: String },
    CancelJob { uri: String, id: i32 },
    ClearJobs { uri: String },
}

/// What a worker thread reports back.
enum PanelUpdate {
    Snapshot {
        generation: u64,
        result: Result<Snapshot, CupsError>,
    },
    Action {
        result: Result<Option<String>, CupsError>,
    },
}

fn perform(client: &CupsClient, action: &Action) -> Result<Option<String>, CupsError> {
    match action {
        Action::TestPage { uri } => client
            .print_test_page(uri)
            .map(|id| Some(format!("Test page submitted as job {id}"))),
        Action::Clean { uri } => client
            .maintenance(uri, "Clean", "Clean print heads")
            .map(|id| Some(format!("Cleaning started as job {id}"))),
        Action::SetDefault { name } => client.set_default(name).map(|()| None),
        Action::SetEnabled { name, enabled } => client.set_enabled(name, *enabled).map(|()| None),
        Action::Remove { name } => client.remove_printer(name).map(|()| None),
        Action::CancelJob { uri, id } => client.cancel_job(uri, *id).map(|()| None),
        Action::ClearJobs { uri } => client.clear_my_jobs(uri).map(|n| {
            if n == 0 {
                None
            } else if n == 1 {
                Some("Cleared 1 job".to_string())
            } else {
                Some(format!("Cleared {n} jobs"))
            }
        }),
    }
}

/// How often the pump checks for worker results while any CUPS
/// query is in flight.
const PUMP_INTERVAL: Duration = Duration::from_millis(50);

/// Maximum concurrent CUPS worker threads admitted per panel. Refresh
/// and action workers share this budget: one refresh plus one action
/// can overlap, and no more.
pub const MAX_PANEL_WORKERS: usize = 2;

/// Bounded admission for panel CUPS workers. GTK-free, so unit tests
/// can prove the bound without a display; the panel owns one and
/// admits every `refresh`/`run` through it. At most
/// [`MAX_PANEL_WORKERS`] workers run at once: an excess refresh
/// coalesces into a single pending refresh (run when a worker reports
/// back), while an excess action is refused and counted.
#[derive(Debug, Default)]
pub struct WorkerGate {
    inflight: usize,
    pending_refresh: bool,
    refused_actions: u64,
}

impl WorkerGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Admit a refresh worker. `true` means spawn; `false` means the
    /// refresh coalesced into the pending one.
    pub fn admit_refresh(&mut self) -> bool {
        if self.inflight < MAX_PANEL_WORKERS {
            self.inflight += 1;
            true
        } else {
            self.pending_refresh = true;
            false
        }
    }

    /// Admit an action worker. `true` means spawn; `false` means the
    /// action was refused and counted.
    pub fn admit_action(&mut self) -> bool {
        if self.inflight < MAX_PANEL_WORKERS {
            self.inflight += 1;
            true
        } else {
            self.refused_actions += 1;
            false
        }
    }

    /// Release one worker. `true` means a coalesced refresh is due:
    /// the caller must launch exactly one refresh.
    pub fn release(&mut self) -> bool {
        self.inflight = self.inflight.saturating_sub(1);
        if self.inflight < MAX_PANEL_WORKERS && self.pending_refresh {
            self.pending_refresh = false;
            true
        } else {
            false
        }
    }

    /// Withdraw an admission whose thread never started.
    pub fn cancel(&mut self) {
        self.inflight = self.inflight.saturating_sub(1);
    }

    /// Workers currently admitted.
    pub fn inflight(&self) -> usize {
        self.inflight
    }

    /// Whether a coalesced refresh waits for a free slot.
    // Test probe; production learns this from `release`.
    #[allow(dead_code)]
    pub fn has_pending(&self) -> bool {
        self.pending_refresh
    }

    /// Excess actions refused since the gate was created.
    // Test probe; production surfaces each refusal in the status line.
    #[allow(dead_code)]
    pub fn refused_actions(&self) -> u64 {
        self.refused_actions
    }
}

/// The printers panel: a header, a status line and one entry per
/// printer. CUPS I/O runs on worker threads, which report back over an
/// `mpsc` channel; a transient main-loop pump (alive only while a
/// query is in flight) applies the results, so the UI never blocks on
/// the daemon.
pub struct PrintersPanel {
    root: gtk::Box,
    list: gtk::Box,
    status: gtk::Label,
    placeholder: gtk::Label,
    client: CupsClient,
    generation: Cell<u64>,
    sender: mpsc::Sender<PanelUpdate>,
    receiver: mpsc::Receiver<PanelUpdate>,
    gate: RefCell<WorkerGate>,
    pumping: Cell<bool>,
}

impl PrintersPanel {
    pub fn new() -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.set_margin_top(12);
        root.set_margin_bottom(12);
        root.set_margin_start(12);
        root.set_margin_end(12);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let title = gtk::Label::new(Some("Printers"));
        title.set_halign(gtk::Align::Start);
        title.set_hexpand(true);
        let refresh = gtk::Button::with_label("Refresh");
        let add = gtk::Button::with_label("Add Printer…");
        add.connect_clicked(|_| open_printers_settings());
        header.append(&title);
        header.append(&refresh);
        header.append(&add);

        let status = gtk::Label::new(None);
        status.set_halign(gtk::Align::Start);
        status.set_wrap(true);
        status.set_visible(false);

        let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let placeholder = gtk::Label::new(Some("Loading…"));
        placeholder.set_halign(gtk::Align::Start);

        root.append(&header);
        root.append(&status);
        root.append(&placeholder);
        root.append(&list);

        let (sender, receiver) = mpsc::channel();
        let panel = Rc::new(Self {
            root,
            list,
            status,
            placeholder,
            client: CupsClient::system(),
            generation: Cell::new(0),
            sender,
            receiver,
            gate: RefCell::new(WorkerGate::new()),
            pumping: Cell::new(false),
        });
        {
            let weak = Rc::downgrade(&panel);
            refresh.connect_clicked(move |_| {
                if let Some(panel) = weak.upgrade() {
                    panel.refresh();
                }
            });
        }
        panel.refresh();
        panel
    }

    /// The panel's root widget, for the shell to place.
    pub fn widget(&self) -> gtk::Box {
        self.root.clone()
    }

    /// Re-read printers and jobs on a worker thread. Past the cap
    /// the refresh coalesces into the pending one instead of spawning.
    pub fn refresh(self: &Rc<Self>) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        if !self.gate.borrow_mut().admit_refresh() {
            return; // At cap: the pending refresh covers this one.
        }
        let client = self.client.clone();
        let sender = self.sender.clone();
        if std::thread::Builder::new()
            .name("tuna-printers".to_string())
            .spawn(move || {
                let result = client.snapshot();
                let _ = sender.send(PanelUpdate::Snapshot { generation, result });
            })
            .is_err()
        {
            self.gate.borrow_mut().cancel();
            self.show_status("Could not start a print query thread");
            return;
        }
        self.ensure_pump();
    }

    /// Run `action` on a worker thread, then refresh. Past the cap the
    /// action is refused (and counted) rather than queued.
    fn run(self: &Rc<Self>, action: Action) {
        if !self.gate.borrow_mut().admit_action() {
            self.show_status("Printer busy; try again shortly");
            return;
        }
        let client = self.client.clone();
        let sender = self.sender.clone();
        if std::thread::Builder::new()
            .name("tuna-printers".to_string())
            .spawn(move || {
                let result = perform(&client, &action);
                let _ = sender.send(PanelUpdate::Action { result });
            })
            .is_err()
        {
            self.gate.borrow_mut().cancel();
            self.show_status("Could not start a print action thread");
            return;
        }
        self.ensure_pump();
    }

    /// Drain worker results on the main loop until every in-flight
    /// query has reported. Only one pump runs at a time.
    fn ensure_pump(self: &Rc<Self>) {
        if self.pumping.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(PUMP_INTERVAL, move || {
            let Some(panel) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            while let Ok(update) = panel.receiver.try_recv() {
                panel.apply(update);
            }
            if panel.gate.borrow().inflight() == 0 {
                panel.pumping.set(false);
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }

    fn show_status(&self, text: &str) {
        self.status.set_text(text);
        self.status.set_visible(true);
    }

    fn apply(self: &Rc<Self>, update: PanelUpdate) {
        let pending = self.gate.borrow_mut().release();
        match update {
            PanelUpdate::Snapshot { generation, result } => {
                if generation != self.generation.get() {
                    // A newer refresh already superseded this one.
                    if pending {
                        self.refresh();
                    }
                    return;
                }
                match result {
                    Ok(snapshot) => {
                        self.status.set_visible(false);
                        self.render(snapshot);
                    }
                    Err(e) => {
                        self.render(Snapshot::default());
                        if e.kind == CupsErrorKind::NoPrintService {
                            self.placeholder.set_text("No Print Service");
                        } else {
                            self.placeholder.set_text("No Printers");
                        }
                        self.show_status(&e.to_string());
                    }
                }
                if pending {
                    self.refresh();
                }
            }
            PanelUpdate::Action { result } => match result {
                Ok(notice) => {
                    if let Some(text) = notice {
                        self.show_status(&text);
                    }
                    // The post-action refresh subsumes any coalesced one.
                    self.refresh();
                }
                Err(e) => {
                    self.show_status(&e.to_string());
                    if pending {
                        self.refresh();
                    }
                }
            },
        }
    }

    fn clear(list: &gtk::Box) {
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
    }

    fn detail_row(parent: &gtk::Box, label: &str, value: &str) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let name = gtk::Label::new(Some(label));
        name.set_halign(gtk::Align::Start);
        name.set_size_request(110, -1);
        let text = gtk::Label::new(Some(value));
        text.set_halign(gtk::Align::Start);
        text.set_hexpand(true);
        text.set_wrap(true);
        row.append(&name);
        row.append(&text);
        parent.append(&row);
    }

    fn action_button(
        panel: &Rc<Self>,
        parent: &gtk::Box,
        label: &str,
        action: Action,
    ) -> gtk::Button {
        let button = gtk::Button::with_label(label);
        let weak = Rc::downgrade(panel);
        button.connect_clicked(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.run(action.clone());
            }
        });
        parent.append(&button);
        button
    }

    fn render(self: &Rc<Self>, snapshot: Snapshot) {
        Self::clear(&self.list);
        self.placeholder.set_visible(snapshot.entries.is_empty());
        if snapshot.entries.is_empty() {
            self.placeholder.set_text("No Printers");
        }
        for entry in snapshot.entries {
            self.render_entry(&entry);
        }
    }

    fn render_entry(self: &Rc<Self>, entry: &PrinterEntry) {
        let printer = &entry.printer;
        let title = if printer.is_default {
            format!("{} (Default)", printer.name)
        } else {
            printer.name.clone()
        };
        let expander = gtk::Expander::builder().label(&title).build();
        // The queue UI shows jobs and actions without an extra click.
        expander.set_expanded(true);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
        body.set_margin_start(12);

        if !printer.make_and_model.is_empty() {
            Self::detail_row(&body, "Model", &printer.make_and_model);
        }
        if !printer.location.is_empty() {
            Self::detail_row(&body, "Location", &printer.location);
        }
        if !printer.device_uri.is_empty() {
            Self::detail_row(&body, "Address", &printer.device_uri);
        }
        let (state, banner) = printer.status();
        Self::detail_row(&body, "Status", &state);
        if let Some(ink) = printer.ink_summary() {
            Self::detail_row(&body, "Ink Level", &ink);
        }
        if let Some(text) = banner {
            let warning = gtk::Label::new(Some(&format!(
                "{text}\nPlease restart when the problem is resolved"
            )));
            warning.set_halign(gtk::Align::Start);
            warning.set_wrap(true);
            body.append(&warning);
        }
        if printer.state == PrinterState::Stopped {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            Self::action_button(
                self,
                &row,
                "Restart",
                Action::SetEnabled {
                    name: printer.name.clone(),
                    enabled: true,
                },
            );
            body.append(&row);
        }

        body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let jobs_title = gtk::Label::new(Some("Active Jobs"));
        jobs_title.set_halign(gtk::Align::Start);
        body.append(&jobs_title);
        let active: Vec<&PrintJob> = entry.jobs.iter().filter(|j| j.state.is_active()).collect();
        if active.is_empty() {
            let none = gtk::Label::new(Some("No Active Printer Jobs"));
            none.set_halign(gtk::Align::Start);
            body.append(&none);
        }
        for job in active {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let mut text = format!("{} — {} — {}", job.name, job.user, job.state.label());
            if job.auth_required() {
                text.push_str(" — Authentication required");
            } else if !job.message.is_empty() {
                text.push_str(&format!(" — {}", job.message));
            }
            let label = gtk::Label::new(Some(&text));
            label.set_halign(gtk::Align::Start);
            label.set_hexpand(true);
            label.set_wrap(true);
            row.append(&label);
            Self::action_button(
                self,
                &row,
                "Cancel",
                Action::CancelJob {
                    uri: printer.uri.clone(),
                    id: job.id,
                },
            );
            body.append(&row);
        }

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_margin_top(6);
        Self::action_button(
            self,
            &actions,
            "Test Page",
            Action::TestPage {
                uri: printer.uri.clone(),
            },
        );
        if printer.commands.iter().any(|c| c == "Clean") {
            Self::action_button(
                self,
                &actions,
                "Clean Print Heads",
                Action::Clean {
                    uri: printer.uri.clone(),
                },
            );
        }
        let default = Self::action_button(
            self,
            &actions,
            "Use Printer by Default",
            Action::SetDefault {
                name: printer.name.clone(),
            },
        );
        default.set_sensitive(!printer.is_default);
        Self::action_button(
            self,
            &actions,
            "Clear All",
            Action::ClearJobs {
                uri: printer.uri.clone(),
            },
        );
        Self::action_button(
            self,
            &actions,
            "Remove Printer",
            Action::Remove {
                name: printer.name.clone(),
            },
        );
        body.append(&actions);

        expander.set_child(Some(&body));
        self.list.append(&expander);
    }
}

// ---------------------------------------------------------------------------
// Quick-settings menu
// ---------------------------------------------------------------------------

/// The menu drains poll results every tick but re-reads CUPS only every
/// [`TILE_POLL_TICKS`] ticks, so rows and subtitle settle within a
/// second of a state change without polling the daemon every second.
const TILE_TICK: Duration = Duration::from_secs(1);
const TILE_POLL_TICKS: u64 = 5;

/// The tile's subtitle for one poll result: the active queue depth when
/// jobs are running, else the printer (or its count), else the honest
/// absence. Failures show the error's short label ("No Print Service",
/// "Authentication Required", ...), matching the panel.
pub fn tile_subtitle(result: &Result<Snapshot, CupsError>) -> String {
    match result {
        Err(e) => e.title().to_string(),
        Ok(snapshot) => {
            let jobs: usize = snapshot
                .entries
                .iter()
                .map(|e| e.jobs.iter().filter(|j| j.state.is_active()).count())
                .sum();
            if jobs > 0 {
                if jobs == 1 {
                    "1 Job".to_string()
                } else {
                    format!("{jobs} Jobs")
                }
            } else {
                match snapshot.entries.as_slice() {
                    [] => "No Printers".to_string(),
                    [entry] => entry.printer.name.clone(),
                    entries => {
                        let count = entries.len();
                        format!("{count} Printers")
                    }
                }
            }
        }
    }
}

/// Compact row label for one printer: name, default mark, status and
/// active queue depth, e.g. "OfficeJet Pro (Default) — Ready — 1 Job".
pub fn row_label(printer: &Printer, jobs: &[PrintJob]) -> String {
    let mut label = printer.name.clone();
    if printer.is_default {
        label.push_str(" (Default)");
    }
    label.push_str(" — ");
    label.push_str(printer.state.label(printer.accepting_jobs));
    let active = jobs.iter().filter(|j| j.state.is_active()).count();
    if active > 0 {
        if active == 1 {
            label.push_str(" — 1 Job");
        } else {
            label.push_str(&format!(" — {active} Jobs"));
        }
    }
    label
}

/// The quick-settings printers menu: one compact row per printer plus
/// the tile's live subtitle, refreshed from CUPS on worker threads. A
/// row opens the queue window with the full panel; the menu itself
/// stays short enough to fit the screen.
pub struct PrintersMenu {
    list: gtk::Box,
    placeholder: gtk::Label,
    tile: RefCell<Option<Tile>>,
    on_open: RefCell<Option<Rc<dyn Fn()>>>,
    panel: Rc<PrintersPanel>,
    window: RefCell<Option<gtk::Window>>,
    client: CupsClient,
    sender: mpsc::Sender<Result<Snapshot, CupsError>>,
    receiver: mpsc::Receiver<Result<Snapshot, CupsError>>,
    inflight: Cell<bool>,
    ticks: Cell<u64>,
}

impl PrintersMenu {
    /// Fill `list` (the menu's runtime box) with compact printer rows.
    pub fn new(list: gtk::Box) -> Rc<Self> {
        let placeholder = gtk::Label::new(Some("Loading…"));
        placeholder.set_halign(gtk::Align::Start);
        list.append(&placeholder);
        let (sender, receiver) = mpsc::channel();
        let menu = Rc::new(Self {
            list,
            placeholder,
            tile: RefCell::new(None),
            on_open: RefCell::new(None),
            panel: PrintersPanel::new(),
            window: RefCell::new(None),
            client: CupsClient::system(),
            sender,
            receiver,
            inflight: Cell::new(false),
            ticks: Cell::new(0),
        });
        menu.tick();
        {
            let menu = menu.clone();
            glib::timeout_add_local(TILE_TICK, move || {
                menu.tick();
                glib::ControlFlow::Continue
            });
        }
        menu
    }

    /// The tile whose subtitle tracks the print service.
    pub fn set_tile(&self, tile: Tile) {
        *self.tile.borrow_mut() = Some(tile);
    }

    /// Run after the queue window opens (the shell closes quick
    /// settings, as menu activation does).
    pub fn set_on_open(&self, on_open: impl Fn() + 'static) {
        *self.on_open.borrow_mut() = Some(Rc::new(on_open));
    }

    /// Re-read CUPS now (the menu calls this when it opens); the
    /// periodic tick keeps rows and subtitle fresh in between. The
    /// shared panel refreshes too, so an open window never goes stale.
    pub fn refresh(self: &Rc<Self>) {
        self.poll();
        self.panel.refresh();
    }

    /// Show the queue window with the full panel, creating it on first
    /// use. The toggle that opened it resets to off.
    pub fn open_window(&self) {
        if let Some(tile) = self.tile.borrow().as_ref() {
            // Momentary toggle: resetting to off emits no handler call
            // that reopens, since the handler only acts when turning on.
            tile.button.set_active(false);
        }
        {
            let mut slot = self.window.borrow_mut();
            if slot.is_none() {
                let window = gtk::Window::new();
                window.set_title(Some("Printers"));
                window.set_default_size(560, 700);
                let header = gtk::HeaderBar::new();
                header.set_title_widget(Some(&gtk::Label::new(Some("Printers"))));
                window.set_titlebar(Some(&header));
                let scrolled = gtk::ScrolledWindow::new();
                scrolled.set_child(Some(&self.panel.widget()));
                scrolled.set_max_content_height(660);
                scrolled.set_propagate_natural_height(true);
                scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
                window.set_child(Some(&scrolled));
                window.connect_close_request(|window| {
                    window.set_visible(false);
                    glib::Propagation::Stop
                });
                *slot = Some(window);
            }
            if let Some(window) = slot.as_ref() {
                window.present();
            }
        }
        let on_open = self.on_open.borrow().clone();
        if let Some(on_open) = on_open {
            on_open();
        }
    }

    fn tick(self: &Rc<Self>) {
        while let Ok(result) = self.receiver.try_recv() {
            self.inflight.set(false);
            self.apply(result);
        }
        let ticks = self.ticks.get() + 1;
        self.ticks.set(ticks);
        if ticks % TILE_POLL_TICKS == 1 {
            self.poll();
        }
    }

    fn poll(&self) {
        if self.inflight.get() {
            return;
        }
        self.inflight.set(true);
        let client = self.client.clone();
        let sender = self.sender.clone();
        if std::thread::Builder::new()
            .name("tuna-printers-menu".to_string())
            .spawn(move || {
                let _ = sender.send(client.snapshot());
            })
            .is_err()
        {
            self.inflight.set(false);
        }
    }

    fn apply(self: &Rc<Self>, result: Result<Snapshot, CupsError>) {
        let subtitle = tile_subtitle(&result);
        if let Some(tile) = self.tile.borrow().as_ref() {
            tile.subtitle.set_text(&subtitle);
            tile.subtitle.set_visible(true);
        }
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        match &result {
            Ok(snapshot) if !snapshot.entries.is_empty() => {
                for entry in &snapshot.entries {
                    self.append_row(entry);
                }
            }
            _ => {
                // Tile and menu agree on the honest absence.
                self.placeholder.set_text(&subtitle);
                self.list.append(&self.placeholder);
            }
        }
    }

    fn append_row(self: &Rc<Self>, entry: &PrinterEntry) {
        let label = row_label(&entry.printer, &entry.jobs);
        let button = gtk::Button::with_label(&label);
        button.add_css_class("qs-menu-item");
        button.update_property(&[gtk::accessible::Property::Label(&label)]);
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            if let Some(menu) = weak.upgrade() {
                menu.open_window();
            }
        });
        self.list.append(&button);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op_group() -> Vec<OutAttr<'static>> {
        vec![
            OutAttr::text("attributes-charset", TAG_CHARSET, "utf-8"),
            OutAttr::text("attributes-natural-language", TAG_LANGUAGE, "en"),
        ]
    }

    #[test]
    fn request_header_layout() {
        let bytes = encode_request(OP_CUPS_GET_PRINTERS, 7, &[(TAG_OPERATION, op_group())]);
        assert_eq!(&bytes[0..8], &[1, 1, 0x40, 0x02, 0, 0, 0, 7]);
        assert_eq!(bytes.last(), Some(&TAG_END));
    }

    #[test]
    fn request_round_trips() {
        let groups = vec![
            (TAG_OPERATION, op_group()),
            (
                TAG_PRINTER,
                vec![
                    OutAttr::text("printer-uri", TAG_URI, "ipp://localhost/printers/a"),
                    OutAttr::int("printer-state", TAG_ENUM, 3),
                    OutAttr::texts("printer-state-reasons", TAG_KEYWORD, &["none"]),
                ],
            ),
        ];
        let bytes = encode_request(OP_CUPS_GET_PRINTERS, 1, &groups);
        let back = decode_response(&bytes).expect("decodes");
        assert_eq!(back.status, OP_CUPS_GET_PRINTERS);
        assert_eq!(back.request_id, 1);
        assert_eq!(back.groups.len(), 2);
        let reasons = back.find(TAG_PRINTER, "printer-state-reasons");
        assert_eq!(reasons.len(), 1);
        assert_eq!(reasons[0].all_texts(), vec!["none"]);
    }

    #[test]
    fn additional_values_accumulate() {
        let bytes = encode_request(
            STATUS_OK,
            1,
            &[(
                TAG_PRINTER,
                vec![OutAttr::texts("reasons", TAG_KEYWORD, &["a", "b"])],
            )],
        );
        let back = decode_response(&bytes).expect("decodes");
        let attr = &back.groups[0].1[0];
        assert_eq!(attr.all_texts(), vec!["a", "b"]);
    }

    #[test]
    fn language_text_decodes_to_text_part() {
        // name-with-language "printer-info": lang "en", text "Hall".
        let mut value = vec![0, 2, b'e', b'n', 0, 4, b'H', b'a', b'l', b'l'];
        let mut bytes = vec![1, 1, 0, 0, 0, 0, 0, 1, TAG_PRINTER];
        bytes.push(TAG_NAME_WITH_LANGUAGE);
        bytes.extend_from_slice(&[0, 12]);
        bytes.extend_from_slice(b"printer-info");
        bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
        bytes.append(&mut value);
        bytes.push(TAG_END);
        let back = decode_response(&bytes).expect("decodes");
        assert_eq!(
            back.find_any("printer-info").and_then(|a| a.first_text()),
            Some("Hall")
        );
    }

    #[test]
    fn truncated_message_errors() {
        let bytes = encode_request(STATUS_OK, 1, &[(TAG_OPERATION, op_group())]);
        for cut in [0, 3, 7, 8, bytes.len() - 1] {
            assert!(
                decode_response(&bytes[..cut]).is_err(),
                "cut at {cut} must fail"
            );
        }
    }

    #[test]
    fn collection_decodes_members() {
        // media-col { media-left-margin 0 }.
        let mut bytes = vec![1, 1, 0, 0, 0, 0, 0, 1, TAG_PRINTER];
        bytes.push(TAG_BEGIN_COLLECTION);
        bytes.extend_from_slice(&[0, 9]);
        bytes.extend_from_slice(b"media-col");
        bytes.extend_from_slice(&[0, 0]);
        bytes.push(0x4a); // memberAttrName
        bytes.extend_from_slice(&[0, 17]);
        bytes.extend_from_slice(b"media-left-margin");
        bytes.extend_from_slice(&[0, 0]);
        bytes.push(TAG_INTEGER);
        bytes.extend_from_slice(&[0, 0, 0, 4, 0, 0, 0, 0]);
        bytes.push(TAG_END_COLLECTION);
        bytes.extend_from_slice(&[0, 0, 0, 0, TAG_END]);
        let back = decode_response(&bytes).expect("decodes");
        let attr = back.find_any("media-col").expect("member group");
        assert!(matches!(attr.values.first(), Some(IppValue::Collection(_))));
    }

    #[test]
    fn post_shape() {
        let body = [1u8, 2, 3];
        let post = build_post("/", &body);
        let text = String::from_utf8_lossy(&post);
        assert!(text.starts_with("POST / HTTP/1.1\r\n"));
        assert!(text.contains("Content-Type: application/ipp\r\n"));
        assert!(text.contains("Content-Length: 3\r\n"));
        assert!(post.ends_with(&body));
    }

    #[test]
    fn head_parses_status_and_length() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\nContent-Length: 12\r\n\r\n";
        let (status, len, length) = parse_response_head(raw).expect("parses");
        assert_eq!((status, length), (200, Some(12)));
        assert_eq!(len, raw.len());
    }

    #[test]
    fn head_without_status_errors() {
        assert!(parse_response_head(b"HTTP/1.1\r\n\r\n").is_err());
        assert!(parse_response_head(b"HTTP/1.1 200 OK\r\n").is_err());
    }

    #[test]
    fn read_response_reads_exact_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nipp!EXTRA";
        let (status, body) = read_response(&mut &raw[..]).expect("reads");
        assert_eq!((status, body), (200, b"ipp!".to_vec()));
    }

    #[test]
    fn read_response_skips_interim() {
        let raw = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n!";
        let (status, body) = read_response(&mut &raw[..]).expect("reads");
        assert_eq!((status, body), (200, b"!".to_vec()));
    }

    #[test]
    fn endpoints_default_and_server() {
        let defaults = resolve_endpoints_from(None);
        assert_eq!(defaults.len(), 3);
        assert_eq!(
            defaults[2],
            Endpoint::Tcp(CUPS_TCP_HOST.to_string(), CUPS_TCP_PORT)
        );
        assert_eq!(
            resolve_endpoints_from(Some("/tmp/cups.sock")),
            vec![Endpoint::Unix(PathBuf::from("/tmp/cups.sock"))]
        );
        assert_eq!(
            resolve_endpoints_from(Some("print:8631")),
            vec![Endpoint::Tcp("print".to_string(), 8631)]
        );
        assert_eq!(
            resolve_endpoints_from(Some("print")),
            vec![Endpoint::Tcp("print".to_string(), CUPS_TCP_PORT)]
        );
        assert_eq!(resolve_endpoints_from(Some("  ")).len(), 3);
    }

    #[test]
    fn user_and_language_fallbacks() {
        assert_eq!(cups_user_from(Some("ada"), None), "ada");
        assert_eq!(cups_user_from(None, Some("lin")), "lin");
        assert_eq!(cups_user_from(Some("  "), None), "anonymous");
        assert_eq!(cups_user_from(None, None), "anonymous");
        assert_eq!(language_from("en_US.UTF-8"), "en-US");
        assert_eq!(language_from("de_DE@euro"), "de-DE");
        assert_eq!(language_from(""), "en");
    }

    #[test]
    fn reason_severity_order() {
        assert_eq!(reason_status("none"), None);
        assert_eq!(reason_status(""), None);
        assert_eq!(reason_status("bogus-error"), None);
        // Error beats warning beats report, whatever the order.
        assert_eq!(
            reason_status("media-low-warning,media-empty-error"),
            Some("Out of paper")
        );
        assert_eq!(
            reason_status("media-empty-report,toner-low-warning"),
            Some("Low on toner")
        );
        assert_eq!(
            reason_status("toner-low-report,media-low-report"),
            Some("Low on toner")
        );
        // Bare reasons count as errors.
        assert_eq!(reason_status("offline"), Some("Offline"));
        assert_eq!(reason_status("paused-error"), Some("Stopped"));
    }

    #[test]
    fn state_labels_match_panel() {
        assert_eq!(PrinterState::Ready.label(true), "Ready");
        assert_eq!(PrinterState::Ready.label(false), "Does not accept jobs");
        assert_eq!(PrinterState::Processing.label(true), "Processing");
        assert_eq!(PrinterState::Stopped.label(true), "Stopped");
    }

    #[test]
    fn banner_only_when_stopped_or_refusing() {
        assert_eq!(
            error_banner(PrinterState::Stopped, true, Some("Out of paper")),
            Some("Out of paper")
        );
        assert_eq!(
            error_banner(PrinterState::Ready, false, Some("Offline")),
            Some("Offline")
        );
        assert_eq!(
            error_banner(PrinterState::Ready, true, Some("Offline")),
            None
        );
        assert_eq!(error_banner(PrinterState::Stopped, true, None), None);
        assert_eq!(error_banner(PrinterState::Stopped, true, Some("")), None);
    }

    #[test]
    fn printer_name_rule() {
        assert!(valid_printer_name("OfficeJet"));
        assert!(valid_printer_name("a-b_c.d~1"));
        assert!(!valid_printer_name(""));
        for bad in ["has space", "has\ttab", "has#hash", "has/slash"] {
            assert!(!valid_printer_name(bad), "{bad:?} must fail");
        }
    }

    fn printer_attrs() -> Vec<IppAttr> {
        vec![
            IppAttr {
                name: "printer-name".into(),
                tag: TAG_NAME,
                values: vec![IppValue::Text("Office".into())],
            },
            IppAttr {
                name: "printer-state".into(),
                tag: TAG_ENUM,
                values: vec![IppValue::Enum(5)],
            },
            IppAttr {
                name: "printer-state-reasons".into(),
                tag: TAG_KEYWORD,
                values: vec![IppValue::Text("media-empty-error".into())],
            },
            IppAttr {
                name: "printer-is-accepting-jobs".into(),
                tag: TAG_BOOLEAN,
                values: vec![IppValue::Boolean(false)],
            },
            IppAttr {
                name: "printer-make-and-model".into(),
                tag: TAG_TEXT,
                values: vec![IppValue::Text("Acme 3000".into())],
            },
            IppAttr {
                name: "marker-names".into(),
                tag: TAG_NAME,
                values: vec![IppValue::Text("Black,Cyan".into())],
            },
            IppAttr {
                name: "marker-levels".into(),
                tag: TAG_INTEGER,
                values: vec![IppValue::Integer(80), IppValue::Integer(-1)],
            },
        ]
    }

    #[test]
    fn printer_maps_and_summarizes() {
        let printer = Printer::from_attrs(&printer_attrs()).expect("printer");
        assert_eq!(printer.name, "Office");
        assert_eq!(printer.uri, "ipp://localhost/printers/Office");
        assert_eq!(printer.state, PrinterState::Stopped);
        assert!(!printer.accepting_jobs);
        let (state, banner) = printer.status();
        assert_eq!(state, "Stopped");
        assert_eq!(banner.as_deref(), Some("Out of paper"));
        assert_eq!(printer.ink_summary().as_deref(), Some("Black 80%, Cyan"));
        assert!(Printer::from_attrs(&[]).is_none());
    }

    #[test]
    fn markers_absent_means_no_ink_row() {
        let attrs = vec![IppAttr {
            name: "printer-name".into(),
            tag: TAG_NAME,
            values: vec![IppValue::Text("Plain".into())],
        }];
        let printer = Printer::from_attrs(&attrs).expect("printer");
        assert_eq!(printer.ink_summary(), None);
        assert_eq!(printer.status().0, "Ready");
    }

    #[test]
    fn job_maps_and_detects_auth_hold() {
        let held = vec![
            IppAttr {
                name: "job-id".into(),
                tag: TAG_INTEGER,
                values: vec![IppValue::Integer(42)],
            },
            IppAttr {
                name: "job-state".into(),
                tag: TAG_ENUM,
                values: vec![IppValue::Enum(4)],
            },
            IppAttr {
                name: "job-hold-until".into(),
                tag: TAG_KEYWORD,
                values: vec![IppValue::Text("auth-info-required".into())],
            },
        ];
        let job = PrintJob::from_attrs(&held).expect("job");
        assert_eq!(job.id, 42);
        assert_eq!(job.name, "Untitled");
        assert!(job.auth_required());
        assert!(job.state.is_active());
        assert!(!JobState::Completed.is_active());
        assert!(PrintJob::from_attrs(&[]).is_none());
    }

    #[test]
    fn uri_escape_and_lpoptions() {
        assert_eq!(uri_escape("Office Jet/2"), "Office%20Jet%2F2");
        assert_eq!(printer_uri("a"), "ipp://localhost/printers/a");
        let text = "Dest Office floor=2\nDefault Office\nDest Lab\n";
        assert_eq!(default_from_lpoptions(text).as_deref(), Some("Office"));
        assert_eq!(default_from_lpoptions("Dest Lab\n"), None);
    }

    #[test]
    fn testprint_search_order() {
        let defaults = testprint_candidates(None);
        assert_eq!(defaults.len(), 4);
        assert_eq!(defaults[0], PathBuf::from("/usr/share/cups/data/testprint"));
        let custom = testprint_candidates(Some("/opt/cups"));
        assert_eq!(custom.len(), 2);
        assert_eq!(custom[0], PathBuf::from("/opt/cups/data/testprint"));
    }

    fn subtitle_job(state: i32) -> PrintJob {
        PrintJob::from_attrs(&[
            IppAttr {
                name: "job-id".into(),
                tag: TAG_INTEGER,
                values: vec![IppValue::Integer(42)],
            },
            IppAttr {
                name: "job-state".into(),
                tag: TAG_ENUM,
                values: vec![IppValue::Enum(state)],
            },
        ])
        .expect("job")
    }

    fn subtitle_error(kind: CupsErrorKind) -> Result<Snapshot, CupsError> {
        Err(CupsError {
            kind,
            message: String::new(),
        })
    }

    #[test]
    fn tile_subtitle_names_queue_printer_or_absence() {
        assert_eq!(
            tile_subtitle(&subtitle_error(CupsErrorKind::NoPrintService)),
            "No Print Service"
        );
        assert_eq!(
            tile_subtitle(&subtitle_error(CupsErrorKind::AuthRequired)),
            "Authentication Required"
        );
        assert_eq!(tile_subtitle(&Ok(Snapshot::default())), "No Printers");
        let office = Printer::from_attrs(&printer_attrs()).expect("printer");
        let one = Snapshot {
            entries: vec![PrinterEntry {
                printer: office.clone(),
                jobs: Vec::new(),
            }],
        };
        assert_eq!(tile_subtitle(&Ok(one)), "Office");
        let mut lab = office.clone();
        lab.name = "Lab".to_string();
        let two = Snapshot {
            entries: vec![
                PrinterEntry {
                    printer: office.clone(),
                    jobs: Vec::new(),
                },
                PrinterEntry {
                    printer: lab,
                    jobs: Vec::new(),
                },
            ],
        };
        assert_eq!(tile_subtitle(&Ok(two)), "2 Printers");
        let queued = Snapshot {
            entries: vec![PrinterEntry {
                printer: office,
                jobs: vec![subtitle_job(5)],
            }],
        };
        assert_eq!(tile_subtitle(&Ok(queued)), "1 Job");
        let done = Snapshot {
            entries: vec![PrinterEntry {
                printer: Printer::from_attrs(&printer_attrs()).expect("printer"),
                jobs: vec![subtitle_job(9)],
            }],
        };
        assert_eq!(tile_subtitle(&Ok(done)), "Office");
    }

    #[test]
    fn row_label_names_printer_status_and_jobs() {
        let office = Printer::from_attrs(&printer_attrs()).expect("printer");
        assert_eq!(row_label(&office, &[]), "Office — Stopped");
        let mut default = office.clone();
        default.is_default = true;
        default.state = PrinterState::Ready;
        default.accepting_jobs = true;
        assert_eq!(
            row_label(&default, &[subtitle_job(5)]),
            "Office (Default) — Ready — 1 Job"
        );
        assert_eq!(
            row_label(&default, &[subtitle_job(5), subtitle_job(3)]),
            "Office (Default) — Ready — 2 Jobs"
        );
        assert_eq!(
            row_label(&default, &[subtitle_job(9)]),
            "Office (Default) — Ready"
        );
    }

    // -- Fake CUPS server ---------------------------------------------------

    fn http_ok(body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }

    fn ok_printers_body(id: u32) -> Vec<u8> {
        encode_request(
            STATUS_OK,
            id,
            &[
                (TAG_OPERATION, op_group()),
                (
                    TAG_PRINTER,
                    vec![
                        OutAttr::text("printer-name", TAG_NAME, "Office"),
                        OutAttr::int("printer-state", TAG_ENUM, 3),
                        OutAttr::texts("printer-state-reasons", TAG_KEYWORD, &["none"]),
                    ],
                ),
                (
                    TAG_PRINTER,
                    vec![
                        OutAttr::text("printer-name", TAG_NAME, "Lab"),
                        OutAttr::int("printer-state", TAG_ENUM, 4),
                    ],
                ),
            ],
        )
    }

    fn ok_jobs_body(id: u32) -> Vec<u8> {
        encode_request(
            STATUS_OK,
            id,
            &[
                (TAG_OPERATION, op_group()),
                (
                    TAG_JOB,
                    vec![
                        OutAttr::int("job-id", TAG_INTEGER, 42),
                        OutAttr::text("job-name", TAG_NAME, "Report"),
                        OutAttr::text("job-originating-user-name", TAG_NAME, "ada"),
                        OutAttr::int("job-state", TAG_ENUM, 5),
                    ],
                ),
            ],
        )
    }

    /// Serve `responses` (complete HTTP replies) in connection order,
    /// recording each raw request. Returns the endpoint, the recorded
    /// requests and the server thread.
    fn start_fake(
        responses: Vec<Vec<u8>>,
    ) -> (
        Endpoint,
        mpsc::Receiver<Vec<u8>>,
        std::thread::JoinHandle<()>,
    ) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind fake CUPS");
        let port = listener.local_addr().expect("port").port();
        let (seen_tx, seen_rx) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            for reply in responses {
                let Ok((mut conn, _)) = listener.accept() else {
                    break;
                };
                conn.set_read_timeout(Some(Duration::from_secs(5))).ok();
                let mut head = Vec::new();
                let mut one = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && head.len() < 65536 {
                    match conn.read(&mut one) {
                        Ok(1) => head.extend_from_slice(&one),
                        _ => break,
                    }
                }
                let length = request_length(&head);
                let mut body = vec![0u8; length];
                let _ = conn.read_exact(&mut body);
                head.extend_from_slice(&body);
                let _ = seen_tx.send(head);
                let _ = conn.write_all(&reply);
            }
        });
        (
            Endpoint::Tcp("127.0.0.1".to_string(), port),
            seen_rx,
            handle,
        )
    }

    fn request_op(request: &[u8]) -> u16 {
        let start = request
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("head")
            + 4;
        u16::from_be_bytes([request[start + 2], request[start + 3]])
    }

    /// The Content-Length of a recorded HTTP request head.
    fn request_length(head: &[u8]) -> usize {
        String::from_utf8_lossy(head)
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.trim().parse().ok())
            .unwrap_or(0)
    }

    #[test]
    fn client_lists_printers_over_http() {
        let (endpoint, seen, server) = start_fake(vec![http_ok(&ok_printers_body(1))]);
        let client = CupsClient::single(endpoint);
        let printers = client.printers().expect("printers");
        assert_eq!(
            request_op(&seen.recv().expect("request")),
            OP_CUPS_GET_PRINTERS
        );
        assert_eq!(printers.len(), 2);
        assert_eq!(printers[0].name, "Office");
        assert_eq!(printers[1].state, PrinterState::Processing);
        server.join().expect("server ends");
    }

    #[test]
    fn client_lists_jobs_and_cancels() {
        // Replies echo each exchange's request id (1..=4 on this client).
        let empty = |id: u32| encode_request(STATUS_OK, id, &[(TAG_OPERATION, op_group())]);
        let (endpoint, seen, server) = start_fake(vec![
            http_ok(&ok_jobs_body(1)),
            http_ok(&empty(2)),
            http_ok(&ok_jobs_body(3)),
            http_ok(&empty(4)),
        ]);
        let client = CupsClient::single(endpoint);
        let jobs = client
            .jobs("ipp://localhost/printers/Office", true)
            .expect("jobs");
        assert_eq!(request_op(&seen.recv().expect("jobs request")), OP_GET_JOBS);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, 42);
        client
            .cancel_job("ipp://localhost/printers/Office", 42)
            .expect("cancel");
        assert_eq!(
            request_op(&seen.recv().expect("cancel request")),
            OP_CANCEL_JOB
        );
        let cleared = client
            .clear_my_jobs("ipp://localhost/printers/Office")
            .expect("clear");
        assert_eq!(cleared, 1);
        server.join().expect("server ends");
    }

    #[test]
    fn client_submits_document_after_encoding() {
        let ok_job = encode_request(
            STATUS_OK,
            1,
            &[
                (TAG_OPERATION, op_group()),
                (TAG_JOB, vec![OutAttr::int("job-id", TAG_INTEGER, 7)]),
            ],
        );
        let (endpoint, seen, server) = start_fake(vec![http_ok(&ok_job)]);
        let client = CupsClient::single(endpoint);
        let id = client
            .maintenance(
                "ipp://localhost/printers/Office",
                "Clean",
                "Clean print heads",
            )
            .expect("command job");
        assert_eq!(id, 7);
        let request = seen.recv().expect("request");
        assert_eq!(request_op(&request), OP_PRINT_JOB);
        assert!(request.ends_with(b"Clean"), "document trails the encoding");
        assert!(request
            .windows(b"Clean print heads".len())
            .any(|w| w == b"Clean print heads"));
        server.join().expect("server ends");
    }

    #[test]
    fn client_maps_http_auth_and_ipp_status() {
        let denied = b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n".to_vec();
        let (endpoint, _, server) = start_fake(vec![denied]);
        let err = CupsClient::single(endpoint).printers().expect_err("401");
        assert_eq!(err.kind, CupsErrorKind::AuthRequired);
        server.join().expect("server ends");

        let forbidden = encode_request(
            STATUS_FORBIDDEN,
            1,
            &[(
                TAG_OPERATION,
                vec![OutAttr::text("status-message", TAG_TEXT, "no way")],
            )],
        );
        let (endpoint, _, server) = start_fake(vec![http_ok(&forbidden)]);
        let err = CupsClient::single(endpoint).printers().expect_err("403");
        assert_eq!(err.kind, CupsErrorKind::Forbidden);
        assert_eq!(err.message, "no way");
        server.join().expect("server ends");
    }

    #[test]
    fn client_reports_no_print_service() {
        let stray = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stray");
        let port = stray.local_addr().expect("port").port();
        drop(stray);
        let err = CupsClient::single(Endpoint::Tcp("127.0.0.1".to_string(), port))
            .printers()
            .expect_err("refused");
        assert_eq!(err.kind, CupsErrorKind::NoPrintService);
        assert_eq!(err.title(), "No Print Service");
    }

    #[test]
    fn server_default_reads_printer_name() {
        let body = encode_request(
            STATUS_OK,
            1,
            &[
                (TAG_OPERATION, op_group()),
                (
                    TAG_PRINTER,
                    vec![OutAttr::text("printer-name", TAG_NAME, "Lab")],
                ),
            ],
        );
        let (endpoint, seen, server) = start_fake(vec![http_ok(&body)]);
        let name = CupsClient::single(endpoint).server_default_name();
        assert_eq!(name.as_deref(), Some("Lab"));
        assert_eq!(
            request_op(&seen.recv().expect("request")),
            OP_CUPS_GET_DEFAULT
        );
        server.join().expect("server ends");
    }

    #[test]
    fn worker_gate_bounds_rapid_refresh_to_cap() {
        // N rapid refresh invocations admit at most MAX_PANEL_WORKERS;
        // the rest coalesce into one pending refresh.
        let mut gate = WorkerGate::new();
        let mut admitted = 0usize;
        let mut peak = 0usize;
        for _ in 0..32 {
            if gate.admit_refresh() {
                admitted += 1;
            }
            peak = peak.max(gate.inflight());
        }
        assert_eq!(admitted, MAX_PANEL_WORKERS);
        assert_eq!(gate.inflight(), MAX_PANEL_WORKERS);
        assert!(peak <= MAX_PANEL_WORKERS);
        assert!(gate.has_pending());
        // Draining one worker surfaces the coalesced refresh exactly
        // once; further releases report nothing pending.
        assert!(gate.release());
        assert!(!gate.has_pending());
        assert!(!gate.release());
        assert_eq!(gate.inflight(), 0);
    }

    #[test]
    fn worker_gate_refuses_excess_actions_and_counts() {
        let mut gate = WorkerGate::new();
        assert!(gate.admit_refresh());
        assert!(gate.admit_action());
        assert!(!gate.admit_action());
        assert!(!gate.admit_action());
        assert_eq!(gate.inflight(), MAX_PANEL_WORKERS);
        assert_eq!(gate.refused_actions(), 2);
        // A freed slot admits again without counting a refusal.
        gate.release();
        assert!(gate.admit_action());
        assert_eq!(gate.refused_actions(), 2);
        // Withdrawing an admission that never spawned frees its slot.
        gate.cancel();
        assert_eq!(gate.inflight(), MAX_PANEL_WORKERS - 1);
    }

    #[test]
    fn client_rejects_reply_with_wrong_request_id() {
        // A well-formed printers reply carrying another request's id is
        // discarded (and counted); the correctly-echoed reply on the
        // same client is accepted, without touching the count.
        let ghost = encode_request(
            STATUS_OK,
            999,
            &[
                (TAG_OPERATION, op_group()),
                (
                    TAG_PRINTER,
                    vec![OutAttr::text("printer-name", TAG_NAME, "Ghost")],
                ),
            ],
        );
        let (endpoint, _, server) =
            start_fake(vec![http_ok(&ghost), http_ok(&ok_printers_body(2))]);
        let client = CupsClient::single(endpoint);
        let err = client.printers().expect_err("wrong id must be rejected");
        assert_eq!(err.kind, CupsErrorKind::Transport);
        assert!(
            err.message.contains("999"),
            "mismatch error names the reply id: {err}"
        );
        assert_eq!(client.id_mismatch_count(), 1);
        let printers = client.printers().expect("correct id accepted");
        assert_eq!(printers.len(), 2);
        assert!(printers.iter().all(|p| p.name != "Ghost"));
        assert_eq!(client.id_mismatch_count(), 1);
        server.join().expect("server ends");
    }

    /// Join `handle` by `deadline`: polls `is_finished` so a stuck
    /// fake server fails the test instead of hanging CI forever.
    fn join_with_deadline(handle: std::thread::JoinHandle<()>, deadline: Instant, what: &str) {
        while !handle.is_finished() {
            if Instant::now() >= deadline {
                panic!("{what} did not finish by its deadline (CI hang prevented)");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        handle.join().expect("server ends");
    }

    /// Serve `plans` in connection order: each `(reply, chunk, delay)`
    /// is written in `chunk`-byte pieces with `delay` between pieces.
    /// A healthy reply uses one chunk and zero delay; a dribble uses
    /// 1-byte chunks with a delay under the per-call limit. Write
    /// errors (the client gave up by its deadline) end that reply and
    /// the server moves to the next connection. Each `accept` waits at
    /// most `accept_timeout`: a peer that never connects ends the
    /// server instead of hanging CI, and tests join with a deadline.
    fn start_dribble(
        plans: Vec<(Vec<u8>, usize, Duration)>,
        accept_timeout: Duration,
    ) -> (Endpoint, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind dribble");
        let port = listener.local_addr().expect("port").port();
        listener.set_nonblocking(true).expect("nonblocking dribble");
        let handle = std::thread::spawn(move || {
            for (reply, chunk_size, delay) in plans {
                let mut accepted = None;
                let wait_start = Instant::now();
                while accepted.is_none() {
                    match listener.accept() {
                        Ok((conn, _)) => {
                            accepted = Some(conn);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            if wait_start.elapsed() >= accept_timeout {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
                let Some(mut conn) = accepted else {
                    break;
                };
                conn.set_nonblocking(false).ok();
                conn.set_read_timeout(Some(Duration::from_secs(5))).ok();
                let mut head = Vec::new();
                let mut one = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && head.len() < 65536 {
                    match conn.read(&mut one) {
                        Ok(1) => head.extend_from_slice(&one),
                        _ => break,
                    }
                }
                let length = request_length(&head);
                let mut body = vec![0u8; length];
                let _ = conn.read_exact(&mut body);
                let chunk_size = chunk_size.max(1);
                let mut offset = 0;
                while offset < reply.len() {
                    let end = offset.saturating_add(chunk_size).min(reply.len());
                    if conn.write_all(&reply[offset..end]).is_err() {
                        break;
                    }
                    offset = end;
                    if offset < reply.len() && !delay.is_zero() {
                        std::thread::sleep(delay);
                    }
                }
            }
        });
        (Endpoint::Tcp("127.0.0.1".to_string(), port), handle)
    }

    #[test]
    fn dribble_terminates_by_absolute_deadline_and_releases_worker() {
        // Per-call 600ms, dribble 200ms per byte (well under per-call,
        // so only the absolute deadline can stop it), absolute 1500ms.
        // The dribbled reply is hundreds of bytes: without the deadline
        // it would hold the worker for over a minute.
        let connect_timeout = Duration::from_millis(500);
        let io_timeout = Duration::from_millis(600);
        let exchange_timeout = Duration::from_millis(1500);
        let dribble_delay = Duration::from_millis(200);
        let dribble = http_ok(&ok_printers_body(1));
        assert!(
            dribble.len() > 50,
            "dribble reply must be many chunks: {}",
            dribble.len()
        );
        let healthy = http_ok(&ok_printers_body(2));
        let (endpoint, server) = start_dribble(
            vec![
                (dribble, 1, dribble_delay),
                (healthy, usize::MAX, Duration::ZERO),
            ],
            Duration::from_secs(10),
        );
        let client = CupsClient::single(endpoint);

        // Occupy one worker slot, as the panel's refresh does.
        let mut gate = WorkerGate::new();
        assert!(gate.admit_refresh());
        assert_eq!(gate.inflight(), 1);

        let start = Instant::now();
        let err = client
            .printers_with_budget(
                start + exchange_timeout,
                connect_timeout,
                io_timeout,
                exchange_timeout,
            )
            .expect_err("dribble must fail");
        let elapsed = start.elapsed();
        assert_eq!(err.kind, CupsErrorKind::Transport);
        assert!(
            err.message.contains("deadline"),
            "must fail by the deadline: {err}"
        );
        // Terminates by the absolute deadline (with slack for CI
        // scheduling), far sooner than the full dribble would take.
        assert!(
            elapsed >= Duration::from_millis(800),
            "must wait for the deadline, not fail fast: {elapsed:?}"
        );
        assert!(
            elapsed < exchange_timeout + Duration::from_secs(5),
            "must terminate by the deadline: {elapsed:?}"
        );

        // Releasing the worker frees the slot ...
        assert!(!gate.release());
        assert_eq!(gate.inflight(), 0);

        // ... and the next healthy exchange succeeds on the freed worker.
        assert!(gate.admit_refresh());
        let printers = client
            .printers_with_budget(
                Instant::now() + exchange_timeout,
                connect_timeout,
                io_timeout,
                exchange_timeout,
            )
            .expect("healthy retry succeeds");
        assert_eq!(printers.len(), 2);
        assert_eq!(printers[0].name, "Office");
        gate.release();
        assert_eq!(gate.inflight(), 0);
        join_with_deadline(
            server,
            Instant::now() + Duration::from_secs(10),
            "dribble server",
        );
    }

    #[test]
    fn snapshot_budget_bounds_serial_job_queries() {
        // Outer snapshot budget 2500ms with a generous 5000ms
        // per-exchange cap, so only the outer budget can stop the
        // dribbled jobs queries. Printers + default answer at once;
        // both jobs queries dribble 1 byte per 200ms (under the 600ms
        // per-call limit).
        let connect_timeout = Duration::from_millis(500);
        let io_timeout = Duration::from_millis(600);
        let exchange_timeout = Duration::from_millis(5000);
        let snapshot_budget = Duration::from_millis(2500);
        let dribble_delay = Duration::from_millis(200);
        let default_body = encode_request(
            STATUS_OK,
            2,
            &[
                (TAG_OPERATION, op_group()),
                (
                    TAG_PRINTER,
                    vec![OutAttr::text("printer-name", TAG_NAME, "Office")],
                ),
            ],
        );
        let (endpoint, server) = start_dribble(
            vec![
                (http_ok(&ok_printers_body(1)), usize::MAX, Duration::ZERO),
                (http_ok(&default_body), usize::MAX, Duration::ZERO),
                (http_ok(&ok_jobs_body(3)), 1, dribble_delay),
            ],
            Duration::from_secs(10),
        );
        // No local `lpoptions` file on a `single` client (injected
        // `None`): the snapshot's second exchange must be the planned
        // server-default reply. No process-global `HOME` mutation.
        let client = CupsClient::single(endpoint);
        let start = Instant::now();
        let snapshot = client.snapshot_with_budget(
            start + snapshot_budget,
            connect_timeout,
            io_timeout,
            exchange_timeout,
        );
        let elapsed = start.elapsed();
        let snapshot = snapshot.expect("printers survive dribbled jobs");
        assert_eq!(snapshot.entries.len(), 2);
        assert!(
            snapshot.entries.iter().all(|e| e.jobs.is_empty()),
            "dribbled jobs must not stall the snapshot"
        );
        assert!(
            elapsed >= Duration::from_millis(1500),
            "must wait for the budget, not fail fast: {elapsed:?}"
        );
        assert!(
            elapsed < snapshot_budget + Duration::from_secs(5),
            "snapshot must terminate by its budget: {elapsed:?}"
        );
        join_with_deadline(
            server,
            Instant::now() + Duration::from_secs(10),
            "dribble server",
        );
    }

    #[test]
    fn dribble_never_connecting_peer_completes() {
        // A fake server whose peer never connects must still terminate:
        // the bounded `accept` times out and the join completes, so CI
        // can never hang on a missing connection.
        let (_endpoint, server) = start_dribble(
            vec![(http_ok(&ok_printers_body(1)), usize::MAX, Duration::ZERO)],
            Duration::from_millis(500),
        );
        let start = Instant::now();
        join_with_deadline(
            server,
            start + Duration::from_secs(5),
            "never-connected dribble server",
        );
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "never-connecting server must end quickly: {elapsed:?}"
        );
        assert!(
            elapsed >= Duration::from_millis(300),
            "must wait for the accept timeout, not fail fast: {elapsed:?}"
        );
    }

    fn server_default_body(id: u32, name: &str) -> Vec<u8> {
        encode_request(
            STATUS_OK,
            id,
            &[
                (TAG_OPERATION, op_group()),
                (
                    TAG_PRINTER,
                    vec![OutAttr::text("printer-name", TAG_NAME, name)],
                ),
            ],
        )
    }

    #[test]
    fn lpoptions_file_prefers_local_default() {
        // A small local file wins without contacting the server: the
        // endpoint is a closed port, so any server lookup would fail.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lpoptions");
        std::fs::write(&path, "Dest Office\nDefault Local\n").expect("write lpoptions");
        let stray = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stray");
        let port = stray.local_addr().expect("port").port();
        drop(stray);
        let client = CupsClient::single(Endpoint::Tcp("127.0.0.1".to_string(), port))
            .with_lpoptions_file(Some(path));
        let name = client.default_name_with_budget(
            Instant::now() + Duration::from_secs(2),
            Duration::from_millis(200),
            Duration::from_millis(200),
            Duration::from_secs(2),
        );
        assert_eq!(name.as_deref(), Some("Local"));
    }

    #[test]
    fn lpoptions_fifo_never_blocks_and_falls_through() {
        // A FIFO at the `lpoptions` path must not block the lookup: the
        // bounded open-first read refuses it immediately (not a regular
        // file) and the server default wins. No writer ever opens the
        // FIFO, so an unbounded `read_to_string` would hang CI forever.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lpoptions");
        let cpath = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).expect("cstring");
        let ret = unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) };
        assert_eq!(ret, 0, "mkfifo must succeed");
        let (endpoint, _, server) = start_fake(vec![http_ok(&server_default_body(1, "Server"))]);
        let client = CupsClient::single(endpoint).with_lpoptions_file(Some(path));
        let start = Instant::now();
        let name = client.default_name_with_budget(
            start + Duration::from_secs(5),
            Duration::from_millis(500),
            Duration::from_millis(500),
            Duration::from_secs(5),
        );
        let elapsed = start.elapsed();
        assert_eq!(name.as_deref(), Some("Server"));
        assert!(
            elapsed < Duration::from_secs(5),
            "FIFO lookup must not block: {elapsed:?}"
        );
        server.join().expect("server ends");
    }

    #[test]
    fn lpoptions_oversize_falls_through_to_server() {
        // Anything over the cap is refused without reading it fully:
        // the server default wins even though the huge file names one.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("lpoptions");
        let mut huge = String::from("Default HugeFile\n");
        while huge.len() <= LPOPTIONS_MAX {
            huge.push_str("Dest Padding opt=a\n");
        }
        std::fs::write(&path, &huge).expect("write huge lpoptions");
        let (endpoint, _, server) = start_fake(vec![http_ok(&server_default_body(1, "Server"))]);
        let client = CupsClient::single(endpoint).with_lpoptions_file(Some(path));
        let start = Instant::now();
        let name = client.default_name_with_budget(
            start + Duration::from_secs(5),
            Duration::from_millis(500),
            Duration::from_millis(500),
            Duration::from_secs(5),
        );
        let elapsed = start.elapsed();
        assert_eq!(name.as_deref(), Some("Server"));
        assert!(
            elapsed < Duration::from_secs(5),
            "oversize lookup must stay quick: {elapsed:?}"
        );
        server.join().expect("server ends");
    }

    #[test]
    fn unix_backlog_blocked_connect_times_out_and_recovers() {
        // A Unix listener that never accepts, with its backlog held
        // full by queued (unaccepted) connects: a production connect
        // with a short budget must fail quickly by that budget — not
        // hang on the backlog — and a subsequent healthy exchange on
        // a freed worker must succeed. No spawned thread outlives.
        use std::os::unix::io::AsRawFd;
        let dir = tempfile::tempdir().expect("tempdir");
        let sock = dir.path().join("backlog.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind unix");
        unsafe {
            libc::listen(listener.as_raw_fd(), 1);
        }
        let mut queued = Vec::new();
        for _ in 0..256 {
            match unix_connect_with_timeout(&sock, Duration::from_millis(50)) {
                Ok(s) => queued.push(s),
                Err(_) => break,
            }
        }
        assert!(
            !queued.is_empty(),
            "backlog fill must queue at least one connect"
        );
        let client = CupsClient::single(Endpoint::Unix(sock.clone()));
        let exchange_timeout = Duration::from_secs(2);
        let start = Instant::now();
        let err = client
            .printers_with_budget(
                start + exchange_timeout,
                Duration::from_millis(300),
                Duration::from_millis(500),
                exchange_timeout,
            )
            .expect_err("backlog-blocked connect must fail");
        let elapsed = start.elapsed();
        assert_eq!(err.kind, CupsErrorKind::NoPrintService);
        assert!(
            elapsed < Duration::from_secs(5),
            "backlog connect must time out, not hang: {elapsed:?}"
        );
        assert!(
            elapsed >= Duration::from_millis(200),
            "must wait for the connect budget, not fail fast: {elapsed:?}"
        );
        drop(queued);
        drop(listener);
        std::fs::remove_file(&sock).ok();
        // Recovery: the next healthy exchange succeeds.
        let (endpoint, _, server) = start_fake(vec![http_ok(&ok_printers_body(1))]);
        let printers = CupsClient::single(endpoint)
            .printers()
            .expect("healthy retry succeeds");
        assert_eq!(printers.len(), 2);
        assert_eq!(printers[0].name, "Office");
        join_with_deadline(
            server,
            Instant::now() + Duration::from_secs(10),
            "recovery server",
        );
    }

    #[test]
    fn slow_subprocess_resolution_times_out_and_recovers() {
        // The kill-and-reap bound: `sleep 10` under a 300ms budget
        // fails in ~300ms with the child killed and reaped before
        // returning (nothing outlives), and healthy work after it —
        // a command and a CUPS exchange — succeeds.
        let start = Instant::now();
        let err = run_command_with_budget(
            "sleep",
            &["10"],
            start + Duration::from_secs(10),
            Duration::from_millis(300),
        )
        .expect_err("sleep must time out");
        let elapsed = start.elapsed();
        assert!(
            err.contains("timed out") || err.contains("deadline"),
            "timeout must be reported: {err}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "slow child must be killed, not awaited: {elapsed:?}"
        );
        assert!(
            elapsed >= Duration::from_millis(200),
            "must wait for the budget, not fail fast: {elapsed:?}"
        );
        let out = run_command_with_budget(
            "echo",
            &["hi"],
            Instant::now() + Duration::from_secs(5),
            Duration::from_secs(2),
        )
        .expect("healthy command succeeds after timeout");
        assert!(out.contains("hi"), "echo output: {out:?}");
        let (endpoint, _, server) = start_fake(vec![http_ok(&ok_printers_body(1))]);
        let printers = CupsClient::single(endpoint)
            .printers()
            .expect("healthy CUPS retry succeeds");
        assert_eq!(printers.len(), 2);
        join_with_deadline(
            server,
            Instant::now() + Duration::from_secs(10),
            "recovery server",
        );
    }

    #[test]
    fn resolve_host_fast_paths_and_failure_stay_bounded() {
        // Literals and `localhost` never touch DNS; an unresolvable
        // name fails quickly through the bounded `getent` path.
        let deadline = Instant::now() + Duration::from_secs(5);
        let addr = resolve_host_with_budget("127.0.0.1", 631, deadline, Duration::from_secs(2))
            .expect("literal resolves");
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert_eq!(addr.port(), 631);
        let addr = resolve_host_with_budget("localhost", 631, deadline, Duration::from_secs(2))
            .expect("localhost resolves");
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        let start = Instant::now();
        let err = resolve_host_with_budget(
            "nonexistent-tuna-test.invalid",
            631,
            start + Duration::from_secs(5),
            Duration::from_secs(2),
        )
        .expect_err("invalid name must fail");
        let elapsed = start.elapsed();
        assert!(!err.is_empty(), "resolution failure must carry a reason");
        assert!(
            elapsed < Duration::from_secs(5),
            "failed resolution must stay quick: {elapsed:?}"
        );
    }

    /// One blackhole connection: accept, then hold without reading
    /// or writing for `hold`, so the peer's large write meets TCP
    /// backpressure. The `accept` waits at most `accept_timeout`, so a
    /// peer that never connects ends the server instead of hanging CI.
    fn start_blackhole(
        accept_timeout: Duration,
        hold: Duration,
    ) -> (Endpoint, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind blackhole");
        let port = listener.local_addr().expect("port").port();
        listener
            .set_nonblocking(true)
            .expect("nonblocking blackhole");
        let handle = std::thread::spawn(move || {
            let wait_start = Instant::now();
            let mut accepted = None;
            while accepted.is_none() {
                match listener.accept() {
                    Ok((conn, _)) => {
                        accepted = Some(conn);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if wait_start.elapsed() >= accept_timeout {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
            if let Some(conn) = accepted {
                std::thread::sleep(hold);
                drop(conn);
            }
        });
        (Endpoint::Tcp("127.0.0.1".to_string(), port), handle)
    }

    /// Mirror the panel's refresh worker: admit through `gate`, spawn a
    /// thread running `task`, and report the `Result` over `sender`.
    /// Tests `recv_timeout` the result (standing in for the GLib pump's
    /// `try_recv`), `release` the gate, and — when `release` reports a
    /// coalesced refresh due — launch the retry, exactly as `apply`
    /// does with `if pending { self.refresh(); }`.
    fn spawn_panel_like_worker<T, F>(
        gate: &mut WorkerGate,
        task: F,
        sender: mpsc::Sender<T>,
    ) -> bool
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        if !gate.admit_refresh() {
            return false;
        }
        if std::thread::Builder::new()
            .name("tuna-printers-test".to_string())
            .spawn(move || {
                let _ = sender.send(task());
            })
            .is_err()
        {
            gate.cancel();
            return false;
        }
        true
    }

    #[test]
    fn worker_release_covers_interim_1xx_through_channel() {
        // A `100 Continue` interim followed by a dribbled final (1
        // byte per 200ms, under the 600ms per-call limit) must still
        // fail by the 1500ms exchange deadline — through a panel-like
        // worker thread and channel, not a direct call — freeing its
        // budget so the GLib-side pending retry succeeds healthy.
        let connect_timeout = Duration::from_millis(500);
        let io_timeout = Duration::from_millis(600);
        let exchange_timeout = Duration::from_millis(1500);
        let mut interim_dribble = b"HTTP/1.1 100 Continue\r\n\r\n".to_vec();
        interim_dribble.extend_from_slice(&http_ok(&ok_printers_body(1)));
        assert!(
            interim_dribble.len() > 50,
            "interim dribble must be many chunks"
        );
        // One plan only: the retry runs against a fresh server
        // below, so a second plan here would wait out its accept
        // timeout for a connection that never comes.
        let (endpoint, server) = start_dribble(
            vec![(interim_dribble, 1, Duration::from_millis(200))],
            Duration::from_secs(10),
        );
        let client = CupsClient::single(endpoint);
        let mut gate = WorkerGate::new();
        let (tx, rx) = mpsc::channel();
        let outer = Instant::now() + exchange_timeout;
        assert!(spawn_panel_like_worker(
            &mut gate,
            move || {
                let client = client;
                client.printers_with_budget(outer, connect_timeout, io_timeout, exchange_timeout)
            },
            tx
        ));
        assert_eq!(gate.inflight(), 1);
        // Fill to the cap-2 with an overlapping action worker, then a
        // further refresh coalesces into the pending one.
        assert!(gate.admit_action());
        assert_eq!(gate.inflight(), 2);
        assert!(!gate.admit_refresh());
        assert!(gate.has_pending());
        let start = Instant::now();
        let result = rx
            .recv_timeout(exchange_timeout + Duration::from_secs(5))
            .expect("worker must report by its deadline");
        let elapsed = start.elapsed();
        let err = result.expect_err("interim dribble must fail");
        assert_eq!(err.kind, CupsErrorKind::Transport);
        assert!(
            err.message.contains("deadline"),
            "must fail by the deadline: {err}"
        );
        assert!(
            elapsed < exchange_timeout + Duration::from_secs(5),
            "must terminate by the deadline: {elapsed:?}"
        );
        // The GLib pump's `release` frees the budget and reports the
        // coalesced refresh due; the retry (as `apply` launches it)
        // succeeds healthy on the freed worker.
        assert!(gate.release());
        assert_eq!(gate.inflight(), 1);
        let (endpoint2, server2) = start_dribble(
            vec![(http_ok(&ok_printers_body(1)), usize::MAX, Duration::ZERO)],
            Duration::from_secs(10),
        );
        let retry_client = CupsClient::single(endpoint2);
        let (tx2, rx2) = mpsc::channel();
        assert!(spawn_panel_like_worker(
            &mut gate,
            move || {
                retry_client.printers_with_budget(
                    Instant::now() + exchange_timeout,
                    connect_timeout,
                    io_timeout,
                    exchange_timeout,
                )
            },
            tx2
        ));
        let printers = rx2
            .recv_timeout(exchange_timeout + Duration::from_secs(5))
            .expect("retry must report")
            .expect("healthy retry succeeds");
        assert_eq!(printers.len(), 2);
        assert!(!gate.release());
        assert!(!gate.release());
        assert_eq!(gate.inflight(), 0);
        join_with_deadline(
            server,
            Instant::now() + Duration::from_secs(10),
            "interim dribble server",
        );
        join_with_deadline(
            server2,
            Instant::now() + Duration::from_secs(10),
            "retry server",
        );
    }

    #[test]
    fn worker_release_covers_body_dribble_through_channel() {
        // Same panel-like worker/channel/pending flow as the interim
        // case, with a plain body dribble: the worker still fails by
        // its deadline, releases, and the pending retry succeeds.
        let connect_timeout = Duration::from_millis(500);
        let io_timeout = Duration::from_millis(600);
        let exchange_timeout = Duration::from_millis(1500);
        let dribble = http_ok(&ok_printers_body(1));
        // One plan only: the retry runs against a fresh server below.
        let (endpoint, server) = start_dribble(
            vec![(dribble, 1, Duration::from_millis(200))],
            Duration::from_secs(10),
        );
        let client = CupsClient::single(endpoint);
        let mut gate = WorkerGate::new();
        let (tx, rx) = mpsc::channel();
        let outer = Instant::now() + exchange_timeout;
        assert!(spawn_panel_like_worker(
            &mut gate,
            move || {
                let client = client;
                client.printers_with_budget(outer, connect_timeout, io_timeout, exchange_timeout)
            },
            tx
        ));
        assert!(gate.admit_action());
        assert!(!gate.admit_refresh());
        let result = rx
            .recv_timeout(exchange_timeout + Duration::from_secs(5))
            .expect("worker must report");
        let err = result.expect_err("body dribble must fail");
        assert_eq!(err.kind, CupsErrorKind::Transport);
        assert!(
            err.message.contains("deadline"),
            "must fail by the deadline: {err}"
        );
        assert!(gate.release());
        assert_eq!(gate.inflight(), 1);
        let (endpoint2, server2) = start_dribble(
            vec![(http_ok(&ok_printers_body(1)), usize::MAX, Duration::ZERO)],
            Duration::from_secs(10),
        );
        let retry_client = CupsClient::single(endpoint2);
        let (tx2, rx2) = mpsc::channel();
        assert!(spawn_panel_like_worker(
            &mut gate,
            move || {
                retry_client.printers_with_budget(
                    Instant::now() + exchange_timeout,
                    connect_timeout,
                    io_timeout,
                    exchange_timeout,
                )
            },
            tx2
        ));
        let printers = rx2
            .recv_timeout(exchange_timeout + Duration::from_secs(5))
            .expect("retry must report")
            .expect("healthy retry succeeds");
        assert_eq!(printers.len(), 2);
        assert!(!gate.release());
        assert!(!gate.release());
        assert_eq!(gate.inflight(), 0);
        join_with_deadline(
            server,
            Instant::now() + Duration::from_secs(10),
            "body dribble server",
        );
        join_with_deadline(
            server2,
            Instant::now() + Duration::from_secs(10),
            "retry server",
        );
    }

    #[test]
    fn worker_release_covers_write_backpressure_through_channel() {
        // A peer that accepts but never reads, met with a 4MiB
        // document: the large write meets TCP backpressure and fails
        // by its budget through the panel-like worker/channel flow,
        // releasing so the pending retry succeeds healthy.
        let connect_timeout = Duration::from_millis(500);
        let io_timeout = Duration::from_millis(600);
        let exchange_timeout = Duration::from_millis(2000);
        let (endpoint, server) = start_blackhole(Duration::from_secs(10), Duration::from_secs(3));
        let client = CupsClient::single(endpoint);
        let mut gate = WorkerGate::new();
        let (tx, rx) = mpsc::channel();
        let outer = Instant::now() + exchange_timeout;
        let document = vec![0x55u8; 4 << 20];
        assert!(spawn_panel_like_worker(
            &mut gate,
            move || {
                let groups = vec![(TAG_OPERATION, op_group())];
                client.exchange_with_budget(
                    OP_PRINT_JOB,
                    groups,
                    Some(&document),
                    outer,
                    connect_timeout,
                    io_timeout,
                )
            },
            tx
        ));
        assert!(gate.admit_action());
        assert!(!gate.admit_refresh());
        let start = Instant::now();
        let result = rx
            .recv_timeout(exchange_timeout + Duration::from_secs(5))
            .expect("backpressured worker must report");
        let elapsed = start.elapsed();
        let err = result.expect_err("backpressured write must fail");
        assert_eq!(err.kind, CupsErrorKind::Transport);
        assert!(
            elapsed < exchange_timeout + Duration::from_secs(5),
            "must terminate by its budget: {elapsed:?}"
        );
        assert!(gate.release());
        assert_eq!(gate.inflight(), 1);
        let (endpoint2, server2) = start_dribble(
            vec![(http_ok(&ok_printers_body(1)), usize::MAX, Duration::ZERO)],
            Duration::from_secs(10),
        );
        let retry_client = CupsClient::single(endpoint2);
        let (tx2, rx2) = mpsc::channel();
        assert!(spawn_panel_like_worker(
            &mut gate,
            move || {
                retry_client.printers_with_budget(
                    Instant::now() + exchange_timeout,
                    connect_timeout,
                    io_timeout,
                    exchange_timeout,
                )
            },
            tx2
        ));
        let printers = rx2
            .recv_timeout(exchange_timeout + Duration::from_secs(5))
            .expect("retry must report")
            .expect("healthy retry succeeds");
        assert_eq!(printers.len(), 2);
        assert!(!gate.release());
        assert!(!gate.release());
        assert_eq!(gate.inflight(), 0);
        join_with_deadline(
            server,
            Instant::now() + Duration::from_secs(10),
            "blackhole server",
        );
        join_with_deadline(
            server2,
            Instant::now() + Duration::from_secs(10),
            "retry server",
        );
    }
}
