//! Launch the actual compositor helper under an isolated, required PAM wrapper.
use std::ffi::CStr;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};
use tuna_compositor::pam_helper::{
    Answer, AnswerText, Event, Identity, Retirement, Style, Supervisor, FLAG,
};

const CHILD: &str = "TUNA_PRIVATE_PAM_TEST_CHILD";
const WRAPPER: &str = "/usr/lib/x86_64-linux-gnu/libpam_wrapper.so";
fn identity() -> Identity {
    Identity {
        nonce: [7; 16],
        request: 42,
        generation: 9,
        uid: unsafe { libc::getuid() },
    }
}
fn user() -> String {
    unsafe { CStr::from_ptr((*libc::getpwuid(libc::getuid())).pw_name) }
        .to_str()
        .unwrap()
        .to_owned()
}
fn executable() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_tuna-compositor"))
}
fn supervisor(mode: &str, budget: Duration) -> Supervisor {
    Supervisor::spawn(
        executable(),
        identity(),
        format!("private-{mode}"),
        user(),
        budget,
    )
    .unwrap()
}
fn answers(sequence: u32) -> Vec<Answer> {
    if sequence == 1 {
        vec![
            Answer {
                index: 0,
                text: Some(AnswerText::new("visible-answer".into())),
            },
            Answer {
                index: 1,
                text: Some(AnswerText::new("first-secret".into())),
            },
            Answer {
                index: 2,
                text: None,
            },
            Answer {
                index: 3,
                text: None,
            },
        ]
    } else {
        vec![Answer {
            index: 0,
            text: Some(AnswerText::new("pin-secret".into())),
        }]
    }
}
fn two_rounds(helper: &mut Supervisor) {
    for sequence in [1, 2] {
        let Event::Challenge {
            sequence: actual,
            messages,
        } = helper.next_event().unwrap()
        else {
            panic!("missing actual PAM challenge")
        };
        assert_eq!(actual, sequence);
        assert_eq!(
            messages
                .iter()
                .map(|message| message.style)
                .collect::<Vec<_>>(),
            if sequence == 1 {
                vec![Style::Visible, Style::Secret, Style::Info, Style::Error]
            } else {
                vec![Style::Secret]
            }
        );
        helper.answer(sequence, answers(sequence)).unwrap();
    }
}
fn retired(helper: &mut Supervisor) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while helper.retry_retirement() != Retirement::Retired && Instant::now() < deadline {}
    assert_eq!(helper.retirement(), Retirement::Retired);
    assert!(
        !PathBuf::from(format!("/proc/{}", helper.pid())).exists(),
        "owned leader not reaped"
    );
}
fn binary_sha() -> String {
    let result = Command::new("sha256sum")
        .arg(executable())
        .output()
        .unwrap();
    assert!(result.status.success());
    let text = String::from_utf8(result.stdout).unwrap();
    let digest = text.split_whitespace().next().unwrap().to_owned();
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    digest
}
fn run_actual_helper_cases() {
    let source_binary = binary_sha();
    println!("private PAM helper executable sha256: {source_binary}");
    for mode in ["multi", "deny", "accountdeny", "userchange"] {
        let mut helper = supervisor(mode, Duration::from_secs(5));
        let actual = std::fs::metadata(format!("/proc/{}/exe", helper.pid())).unwrap();
        let built = std::fs::metadata(executable()).unwrap();
        assert_eq!((actual.dev(), actual.ino()), (built.dev(), built.ino()));
        let cmdline = std::fs::read(format!("/proc/{}/cmdline", helper.pid())).unwrap();
        assert!(!String::from_utf8_lossy(&cmdline).contains("secret"));
        two_rounds(&mut helper);
        let Event::Finished { authenticated } = helper.next_event().unwrap() else {
            panic!("missing terminal result")
        };
        assert_eq!(authenticated, mode == "multi");
        retired(&mut helper);
    }
    for mode in ["hang", "crash", "teardown"] {
        let started = Instant::now();
        let mut helper = supervisor(mode, Duration::from_millis(750));
        if mode == "teardown" {
            two_rounds(&mut helper);
        }
        assert!(
            helper.next_event().is_err(),
            "hung/crashed transaction accepted"
        );
        retired(&mut helper);
        assert!(started.elapsed() < Duration::from_secs(4));
    }
    for bad in ["cancel", "stale", "duplicate", "oversized", "notice-answer"] {
        let mut helper = supervisor("multi", Duration::from_secs(5));
        let Event::Challenge { sequence, .. } = helper.next_event().unwrap() else {
            panic!("missing challenge")
        };
        if bad == "cancel" {
            helper.cancel();
        } else {
            let mut values = answers(sequence);
            if bad == "duplicate" {
                values[1].index = 0;
            }
            if bad == "oversized" {
                values[1].text = Some(AnswerText::new("x".repeat(4097)));
            }
            if bad == "notice-answer" {
                values[2].text = Some(AnswerText::new("not permitted".into()));
            }
            assert!(helper
                .answer(
                    if bad == "stale" {
                        sequence + 1
                    } else {
                        sequence
                    },
                    values
                )
                .is_err());
        }
        retired(&mut helper);
    }
    let mut helper = supervisor("child", Duration::from_secs(5));
    two_rounds(&mut helper);
    let Event::Challenge { sequence, messages } = helper.next_event().unwrap() else {
        panic!("missing child notice")
    };
    let descendant: u32 = messages[0]
        .text
        .strip_prefix("Owned child PID ")
        .unwrap()
        .parse()
        .unwrap();
    helper
        .answer(
            sequence,
            vec![Answer {
                index: 0,
                text: None,
            }],
        )
        .unwrap();
    assert!(
        helper.next_event().is_err(),
        "result accepted while group descendant was alive"
    );
    retired(&mut helper);
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{descendant}/stat")) {
        assert!(
            stat.rsplit_once(')')
                .unwrap()
                .1
                .trim_start()
                .starts_with('Z'),
            "owned descendant still alive"
        );
    }
    assert!(!format!("{:?}", AnswerText::new("first-secret".into())).contains("first-secret"));
    // The process remains alive while the creating thread terminates. Do not
    // send cancel/kill: observe the kernel parent-thread death before retiring.
    let mut helper = std::thread::spawn(|| supervisor("multi", Duration::from_secs(5)))
        .join()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", helper.pid())).unwrap();
        if stat
            .rsplit_once(')')
            .unwrap()
            .1
            .trim_start()
            .starts_with('Z')
        {
            break;
        }
        if Instant::now() >= deadline {
            helper.cancel();
            panic!("helper outlived its creating thread");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(Path::new("/proc/self").exists());
    assert_eq!(helper.retry_retirement(), Retirement::Retired);
    assert!(helper.next_event().is_err());
    raw_negative_cases();
    assert_eq!(binary_sha(), source_binary);
}
struct RawChild {
    child: Child,
    reaped: bool,
}
impl Drop for RawChild {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        // Reserve the unreaped leader while signalling its owned group.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                self.reaped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
fn raw_read<T: Read + AsRawFd>(
    output: &mut T,
    bytes: &mut [u8],
    deadline: Instant,
) -> io::Result<()> {
    let fd = output.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut offset = 0;
    while offset < bytes.len() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(io::ErrorKind::TimedOut)?;
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let count = unsafe {
            libc::poll(
                &mut poll,
                1,
                remaining
                    .as_millis()
                    .saturating_add(1)
                    .min(i32::MAX as u128) as i32,
            )
        };
        if count == 0 {
            return Err(io::ErrorKind::TimedOut.into());
        }
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        match output.read(&mut bytes[offset..]) {
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
fn raw_body(output: &mut ChildStdout, deadline: Instant) -> Vec<u8> {
    let mut header = [0; 4];
    raw_read(output, &mut header, deadline).unwrap();
    let count = u32::from_le_bytes(header);
    assert!((1..=65536).contains(&count));
    let mut body = vec![0; count as usize];
    raw_read(output, &mut body, deadline).unwrap();
    body
}
fn raw_frame(input: &mut impl Write, value: &serde_json::Value) {
    let bytes = serde_json::to_vec(value).unwrap();
    input
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .unwrap();
    input.write_all(&bytes).unwrap();
}
fn raw_negative_cases() {
    for mode in [
        "oversized-header",
        "wrong-uid",
        "stale-generation",
        "dribble",
    ] {
        let parent = unsafe { libc::getpid() };
        let mut command = Command::new(executable());
        command
            .arg(FLAG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
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
        let mut owned = RawChild {
            child: command.spawn().unwrap(),
            reaped: false,
        };
        let child = &mut owned.child;
        let transport_deadline = Instant::now() + Duration::from_secs(2);
        let mut input = child.stdin.take().unwrap();
        let mut output = child.stdout.take().unwrap();
        let mut token = serde_json::to_value(identity()).unwrap();
        if mode == "oversized-header" {
            input.write_all(&65537u32.to_le_bytes()).unwrap();
        } else {
            if mode == "wrong-uid" {
                token["uid"] = serde_json::json!(identity().uid.wrapping_add(1));
            }
            raw_frame(
                &mut input,
                &serde_json::json!({"kind":"start","identity":token,"service":"private-multi","user":user(),"budget_ms":300}),
            );
            if matches!(mode, "stale-generation" | "dribble") {
                let _challenge = raw_body(&mut output, transport_deadline);
                if mode == "stale-generation" {
                    token["generation"] = serde_json::json!(10);
                    raw_frame(
                        &mut input,
                        &serde_json::json!({"kind":"answers","identity":token,"sequence":1,"answers":answers(1)}),
                    );
                    let body = raw_body(&mut output, transport_deadline);
                    assert_eq!(
                        serde_json::from_slice::<serde_json::Value>(&body).unwrap()
                            ["authenticated"],
                        false
                    );
                } else {
                    input.write_all(&1000u32.to_le_bytes()).unwrap();
                    let dribble_end = Instant::now() + Duration::from_millis(800);
                    while Instant::now() < dribble_end {
                        if input.write_all(b" ").is_err() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            }
        }
        if matches!(mode, "oversized-header" | "wrong-uid") {
            // The helper rejects on its own. Its conversation EOF must come from
            // the kernel's exit-time close, which runs only after the process
            // released its address space (stat vsize 0), never from an early
            // close while exit work still runs: Supervisor kills the group on
            // EOF, which would cut that work short (an instrumented build
            // writes its coverage profile there).
            let error = raw_read(&mut output, &mut [0], transport_deadline).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
            let stat = std::fs::read_to_string(format!("/proc/{}/stat", child.id()))
                .expect("unreaped helper keeps its /proc entry");
            let vsize: u64 = stat
                .rsplit_once(')')
                .unwrap()
                .1
                .split_whitespace()
                .nth(20)
                .unwrap()
                .parse()
                .unwrap();
            assert_eq!(
                vsize, 0,
                "{mode}: helper closed its conversation while its process still ran"
            );
        }
        drop(input);
        let deadline = Instant::now() + Duration::from_secs(2);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                owned.reaped = true;
                break status;
            }
            if Instant::now() >= deadline {
                panic!("raw helper failed absolute deadline");
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        if mode != "stale-generation" {
            assert!(!status.success());
        }
        let mut stderr = child.stderr.take().unwrap();
        let error = raw_read(
            &mut stderr,
            &mut [0],
            Instant::now() + Duration::from_millis(250),
        )
        .unwrap_err();
        assert_eq!(
            error.kind(),
            io::ErrorKind::UnexpectedEof,
            "helper diagnostics or unclosed diagnostic stream"
        );
    }
}
#[test]
fn private_pam_helper_actual_binary_conversation_and_retirement() {
    if std::env::var_os(CHILD).is_some() {
        run_actual_helper_cases();
        run_backend_cases();
        return run_interactive_relay_cases();
    }
    if !Path::new(WRAPPER).exists() {
        assert!(
            std::env::var_os("TUNA_REQUIRE_PAM_WRAPPER").is_none(),
            "required PAM wrapper missing"
        );
        eprintln!("private helper proof unavailable: PAM wrapper missing");
        return;
    }
    let scratch = tempfile::tempdir().unwrap();
    let module = scratch.path().join("controlled-pam.so");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pam_conversation.c");
    assert!(Command::new("cc")
        .args(["-shared", "-fPIC", "-O2", "-Wall", "-Wextra", "-Werror"])
        .arg(source)
        .arg("-o")
        .arg(&module)
        .arg("-lpam")
        .status()
        .unwrap()
        .success());
    let services = scratch.path().join("services");
    std::fs::create_dir(&services).unwrap();
    for mode in [
        "single",
        "single-deny",
        "single-accountdeny",
        "repeat",
        "multi",
        "deny",
        "accountdeny",
        "hang",
        "crash",
        "teardown",
        "child",
        "userchange",
    ] {
        std::fs::write(
            services.join(format!("private-{mode}")),
            format!(
                "auth required {} {mode}\naccount required {} {mode}\n",
                module.display(),
                module.display()
            ),
        )
        .unwrap();
    }
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "private_pam_helper_actual_binary_conversation_and_retirement",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("LD_PRELOAD", WRAPPER)
        .env("PAM_WRAPPER", "1")
        .env("PAM_WRAPPER_SERVICE_DIR", services)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(45);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("controlled proof process exceeded its deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success(), "controlled actual helper proof failed");
}

// The library's Rust test image cannot serve the private entrypoint. This
// explicit trusted factory uses the same admitted built compositor as above.
fn run_backend_cases() {
    use tuna_compositor::pam_backend::{Backend, Verification};
    use tuna_shell_control::Secret;
    let (created, creators) = std::sync::mpsc::channel();
    let backend = Backend::with_trusted_factory(move |identity, service, user, budget, cancel| {
        let helper =
            Supervisor::spawn_cancellable(executable(), identity, service, user, budget, cancel)?;
        created
            .send((
                std::thread::current().id(),
                unsafe { libc::syscall(libc::SYS_gettid) },
                helper.pid(),
            ))
            .unwrap();
        Ok(helper)
    })
    .unwrap();
    let mut creator = None;
    for (mode, password, accepted) in [
        ("single", "right-password", true),
        ("single", "wrong-password", false),
        ("single-deny", "right-password", false),
        ("single-accountdeny", "right-password", false),
        ("repeat", "right-password", false),
        ("single", "right-password", true),
    ] {
        println!("stable PAM creator case: {mode}");
        let ticket = backend
            .begin(
                format!("private-{mode}"),
                user(),
                Secret(password.into()),
                tuna_compositor::pam_cancel::Cancellation::default(),
            )
            .unwrap();
        let (actual, _, _) = creators.recv_timeout(Duration::from_secs(2)).unwrap();
        if let Some(original) = creator {
            assert_eq!(original, actual);
        } else {
            creator = Some(actual);
        }
        let mut pending_count = 0;
        assert_eq!(
            ticket.wait_retired(|| pending_count += 1),
            Verification::Retired {
                authenticated: accepted
            },
            "case {mode} requires an actual retired result"
        );
        assert_eq!(
            pending_count,
            usize::from(mode == "repeat"),
            "only the unsupported second secret may emit one pending denial"
        );
    }
    // Busy admission cannot queue another credential. Caller/ticket cancellation
    // leaves the creator owning its helper until the physical Retired receipt.
    let cancel = tuna_compositor::pam_cancel::Cancellation::default();
    let ticket = backend
        .begin(
            "private-hang".into(),
            user(),
            Secret("right-password".into()),
            cancel.clone(),
        )
        .unwrap();
    let (actual, tid, pid) = creators.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(actual, creator.unwrap());
    wait_backend_idle_read(tid);
    assert!(matches!(
        backend.begin(
            "private-single".into(),
            user(),
            Secret("second".into()),
            tuna_compositor::pam_cancel::Cancellation::default()
        ),
        Err(Verification::RejectedBeforeSpawn)
    ));
    let started = Instant::now();
    cancel.cancel();
    let mut pending = false;
    assert_eq!(
        ticket.wait_retired(|| pending = true),
        Verification::Retired {
            authenticated: false
        }
    );
    assert!(pending);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "idle helper cancellation waited for its ten-second deadline"
    );
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "Retired must follow actual helper reap"
    );
    // A wake retained by the previous attempt must not deny its successor.
    cancel.cancel();
    let ticket = backend
        .begin(
            "private-single".into(),
            user(),
            Secret("right-password".into()),
            tuna_compositor::pam_cancel::Cancellation::default(),
        )
        .unwrap();
    assert_eq!(
        ticket.wait_retired(|| panic!("unexpected post-retirement pending")),
        Verification::Retired {
            authenticated: true
        }
    );
    let _ = creators.recv_timeout(Duration::from_secs(2)).unwrap();
    // Exercise shutdown and ticket-drop on an actual blocked helper read too.
    let ticket = backend
        .begin(
            "private-hang".into(),
            user(),
            Secret("right-password".into()),
            tuna_compositor::pam_cancel::Cancellation::default(),
        )
        .unwrap();
    let (_, tid, pid) = creators.recv_timeout(Duration::from_secs(2)).unwrap();
    wait_backend_idle_read(tid);
    let started = Instant::now();
    backend.shutdown();
    assert_eq!(
        ticket.wait_retired(|| {}),
        Verification::Retired {
            authenticated: false
        }
    );
    wait_helper_reaped(pid, started);
    let (created, creators) = std::sync::mpsc::channel();
    let dropped_backend =
        Backend::with_trusted_factory(move |identity, service, user, budget, cancel| {
            let helper = Supervisor::spawn_cancellable(
                executable(),
                identity,
                service,
                user,
                budget,
                cancel,
            )?;
            created
                .send((unsafe { libc::syscall(libc::SYS_gettid) }, helper.pid()))
                .unwrap();
            Ok(helper)
        })
        .unwrap();
    let ticket = dropped_backend
        .begin(
            "private-hang".into(),
            user(),
            Secret("right-password".into()),
            tuna_compositor::pam_cancel::Cancellation::default(),
        )
        .unwrap();
    let (tid, pid) = creators.recv_timeout(Duration::from_secs(2)).unwrap();
    wait_backend_idle_read(tid);
    let started = Instant::now();
    drop(ticket);
    wait_helper_reaped(pid, started);
    dropped_backend.shutdown();
    println!("stable PAM creator: single/wrong/account/repeated-secret/busy/cancel/actual-retirement controls passed");
}

// Observe the stable creator actually blocked in the production two-fd poll
// after Start was sent, rather than canceling before it enters helper I/O.
fn wait_backend_idle_read(tid: libc::c_long) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let state = std::fs::read_to_string(format!("/proc/self/task/{tid}/syscall")).unwrap();
        let fields: Vec<_> = state.split_whitespace().collect();
        let syscall = fields.first().and_then(|v| v.parse::<libc::c_long>().ok());
        let polling = syscall == Some(libc::SYS_ppoll);
        #[cfg(target_arch = "x86_64")]
        let polling = polling || syscall == Some(libc::SYS_poll);
        if polling && fields.get(2).copied() == Some("0x2") {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("creator did not enter the actual cancellable helper read poll");
}
fn wait_helper_reaped(pid: u32, started: Instant) {
    while Path::new(&format!("/proc/{pid}")).exists() {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "cancellation did not retire idle helper promptly"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

// Reuses the admitted actual main executable and isolated controlled PAM module.
// This is backend conversation evidence, not a Runtime Slot/UI/hardware proof.
fn run_interactive_relay_cases() {
    use tuna_compositor::pam_backend::{Backend, ConversationTicket, Verification};
    use tuna_compositor::pam_cancel::Cancellation;
    use tuna_compositor::pam_conversation::{Binding, Challenge, Origin, Policy};
    fn next(
        ticket: &mut ConversationTicket,
        event_loop: &mut calloop::EventLoop<usize>,
        wakes: &mut usize,
    ) -> Challenge {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let challenge = ticket.try_challenge();
            assert!(
                Instant::now() < deadline,
                "challenge observation exceeded deadline"
            );
            if let Ok(challenge) = challenge {
                return challenge;
            }
            let outcome = ticket.try_outcome();
            assert!(
                Instant::now() < deadline,
                "outcome observation exceeded challenge deadline"
            );
            assert!(outcome.is_none(), "unexpected retirement before challenge");
            assert!(Instant::now() < deadline, "required challenge missing");
            event_loop
                .dispatch(
                    Some(deadline.saturating_duration_since(Instant::now())),
                    wakes,
                )
                .unwrap();
        }
    }
    fn finished(
        ticket: &mut ConversationTicket,
        accepted: bool,
        event_loop: &mut calloop::EventLoop<usize>,
        wakes: &mut usize,
    ) {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            let outcome = ticket.try_outcome();
            assert!(
                Instant::now() < deadline,
                "retirement observation exceeded deadline"
            );
            if let Some(result) = outcome {
                assert_eq!(
                    result,
                    Verification::Retired {
                        authenticated: accepted
                    }
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "required actual retirement missing"
            );
            event_loop
                .dispatch(
                    Some(deadline.saturating_duration_since(Instant::now())),
                    wakes,
                )
                .unwrap();
        }
    }
    let (created, creators) = std::sync::mpsc::channel();
    let backend = Backend::with_trusted_factory(move |identity, service, user, total, cancel| {
        let helper =
            Supervisor::spawn_conversation(executable(), identity, service, user, total, cancel)?;
        let actual = std::fs::metadata(format!("/proc/{}/exe", helper.pid()))?;
        let expected = std::fs::metadata(executable())?;
        assert_eq!(
            (actual.dev(), actual.ino()),
            (expected.dev(), expected.ino())
        );
        created
            .send((
                std::thread::current().id(),
                unsafe { libc::syscall(libc::SYS_gettid) },
                helper.pid(),
            ))
            .unwrap();
        Ok(helper)
    })
    .unwrap();
    let mut creator = None;
    for (attempt, case) in [
        "fresh-multistep",
        "wrong-second",
        "wrong-origin",
        "wrong-request",
        "wrong-epoch",
        "wrong-sequence",
        "cancel-pending",
        "phase-hang",
        "fresh-multistep",
        "shutdown-pending",
    ]
    .into_iter()
    .enumerate()
    {
        println!("interactive PAM relay case: {case}");
        let binding = Binding {
            origin: Origin {
                connection: 70,
                request: attempt as u64 + 1,
            },
            attempt: attempt as u64 + 1,
            epoch: 50 + attempt as u64,
        };
        let cancel = Cancellation::default();
        let mut ticket = backend
            .begin_conversation(
                if case == "phase-hang" {
                    "private-hang"
                } else {
                    "private-multi"
                }
                .into(),
                user(),
                binding,
                Policy {
                    backend_phase: Duration::from_secs(if attempt == 0 { 30 } else { 2 }),
                    total: Duration::from_secs(if attempt == 0 { 900 } else { 300 }),
                },
                cancel.clone(),
            )
            .unwrap();
        let mut event_loop = calloop::EventLoop::<usize>::try_new().unwrap();
        let source = ticket
            .take_notice_source()
            .expect("owned conversation notice source");
        assert!(
            ticket.take_notice_source().is_none(),
            "source cannot be duplicated"
        );
        assert!(ticket.next_deadline().is_some());
        event_loop
            .handle()
            .insert_source(source, |_, _, wakes| *wakes += 1)
            .unwrap();
        let mut wakes = 0;
        let (actual_creator, tid, pid) = creators.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(*creator.get_or_insert(actual_creator), actual_creator);
        assert!(
            backend
                .begin_conversation(
                    "private-multi".into(),
                    user(),
                    Binding {
                        attempt: binding.attempt + 1,
                        ..binding
                    },
                    Policy {
                        backend_phase: Duration::from_secs(2),
                        total: Duration::from_secs(300)
                    },
                    Cancellation::default()
                )
                .is_err(),
            "shared backend admission must remain busy"
        );
        if case != "phase-hang" {
            let mut challenge = next(&mut ticket, &mut event_loop, &mut wakes);
            assert_eq!(challenge.binding, binding);
            assert_eq!(challenge.sequence, 1);
            assert_eq!(
                challenge
                    .messages
                    .iter()
                    .map(|message| message.style)
                    .collect::<Vec<_>>(),
                [Style::Visible, Style::Secret, Style::Info, Style::Error]
            );
            match case {
                "wrong-origin" | "wrong-request" | "wrong-epoch" => {
                    let incoming = match case {
                        "wrong-origin" => Binding {
                            origin: Origin {
                                connection: binding.origin.connection + 1,
                                ..binding.origin
                            },
                            ..binding
                        },
                        "wrong-request" => Binding {
                            origin: Origin {
                                request: binding.origin.request + 1,
                                ..binding.origin
                            },
                            ..binding
                        },
                        _ => Binding {
                            epoch: binding.epoch + 1,
                            ..binding
                        },
                    };
                    assert!(challenge.response.submit(incoming, 1, answers(1)).is_err());
                }
                "wrong-sequence" => {
                    assert!(challenge.response.submit(binding, 2, answers(1)).is_err());
                }
                "cancel-pending" | "shutdown-pending" => {
                    wait_backend_idle_read(tid);
                    if case == "shutdown-pending" {
                        backend.shutdown();
                    } else {
                        ticket.cancel();
                    }
                    assert!(challenge.response.submit(binding, 1, answers(1)).is_err());
                }
                _ => {
                    challenge.response.note_user_input(binding, 1).unwrap();
                    challenge.response.submit(binding, 1, answers(1)).unwrap();
                    let challenge = next(&mut ticket, &mut event_loop, &mut wakes);
                    assert_eq!(challenge.binding, binding);
                    assert_eq!(challenge.sequence, 2);
                    assert_eq!(challenge.messages.len(), 1);
                    assert_eq!(challenge.messages[0].style, Style::Secret);
                    let values = if case == "wrong-second" {
                        vec![Answer {
                            index: 0,
                            text: Some(AnswerText::new("first-secret".into())),
                        }]
                    } else {
                        answers(2)
                    };
                    challenge.response.submit(binding, 2, values).unwrap();
                }
            }
        }
        finished(
            &mut ticket,
            case == "fresh-multistep",
            &mut event_loop,
            &mut wakes,
        );
        event_loop
            .dispatch(Some(Duration::ZERO), &mut wakes)
            .unwrap();
        assert!(
            wakes > 0,
            "actual helper conversation must wake its owned consumer"
        );
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "result must follow actual helper retirement"
        );
    }
    backend.shutdown();
    println!("interactive PAM relay: distinct fresh answers, Info/Error, owner/sequence, cancel, phase deadline, stable creator and physical retirement controls passed");
}
