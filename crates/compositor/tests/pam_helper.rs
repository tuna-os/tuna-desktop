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
        return run_actual_helper_cases();
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
