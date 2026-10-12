//! Supervised renderer-only proof peer. No production wellbeing policy claims.
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};
use tuna_shell_control::{
    decode_frame, encode_frame, CommandKind, CommandStatus, InputSettings, Message, MotionLevel,
    MotionPolicy, CURRENT_VERSION, MAX_FRAME_BYTES,
};

fn receive(stream: &mut UnixStream) -> std::io::Result<Message> {
    let mut prefix = [0; 4];
    stream.read_exact(&mut prefix)?;
    let n = u32::from_le_bytes(prefix) as usize;
    if n > MAX_FRAME_BYTES {
        return Err(std::io::Error::other("oversize control frame"));
    }
    let mut frame = vec![0; n + 4];
    frame[..4].copy_from_slice(&prefix);
    stream.read_exact(&mut frame[4..])?;
    decode_frame(&frame).map_err(std::io::Error::other)
}
fn command(stream: &mut UnixStream, replies: &mpsc::Receiver<Message>, id: u64, kind: CommandKind) {
    stream
        .write_all(&encode_frame(&Message::Command { id, kind }))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let message = replies
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("control command timed out");
        if let Message::CommandResult { id: seen, status } = message {
            if seen == id {
                assert_eq!(status, CommandStatus::Applied);
                break;
            }
        }
    }
}
fn main() {
    let parent = unsafe { libc::getppid() };
    let path = PathBuf::from(std::env::var_os("TUNA_WELLBEING_COMMAND").unwrap());
    let reply = PathBuf::from(std::env::var_os("TUNA_WELLBEING_REPLY").unwrap());
    let socket = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap())
        .join("tuna-wellbeing-proof.control");
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .write_all(&encode_frame(&Message::Hello {
            version: CURRENT_VERSION,
        }))
        .unwrap();
    match receive(&mut stream).unwrap() {
        Message::Hello { version } => assert_eq!(version, CURRENT_VERSION),
        _ => panic!("missing hello"),
    }
    match receive(&mut stream).unwrap() {
        Message::Snapshot { locked: false, .. } => {}
        _ => panic!("missing unlocked snapshot"),
    }
    let mut reader = stream.try_clone().unwrap();
    let (sender, replies) = mpsc::channel();
    std::thread::spawn(move || {
        while let Ok(message) = receive(&mut reader) {
            if sender.send(message).is_err() {
                break;
            }
        }
    });
    let mut settings = InputSettings::default();
    command(
        &mut stream,
        &replies,
        1,
        CommandKind::SetInputSettings(settings.clone().into()),
    );
    command(
        &mut stream,
        &replies,
        2,
        CommandKind::SetWellbeingEffects {
            grayscale: false,
            fade: false,
        },
    );
    println!(
        "{{\"ready\":true,\"pid\":{},\"parent\":{},\"protocol\":35}}",
        std::process::id(),
        parent
    );
    let bus = zbus::blocking::Connection::session().unwrap();
    let mut generation = 0;
    loop {
        if unsafe { libc::getppid() } != parent {
            break;
        }
        // Drain unsolicited snapshots/deltas so they cannot accumulate indefinitely.
        while replies.try_recv().is_ok() {}
        if let Ok(bytes) = std::fs::read(&path) {
            assert!(bytes.len() <= 4096, "proof command too large");
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let next = value["generation"].as_u64().unwrap();
            if next > generation {
                assert_eq!(next, generation + 1, "proof generation skipped");
                match value["action"].as_str().unwrap() {
                    "effects" => command(
                        &mut stream,
                        &replies,
                        next + 100,
                        CommandKind::SetWellbeingEffects {
                            grayscale: value["grayscale"].as_bool().unwrap(),
                            fade: value["fade"].as_bool().unwrap(),
                        },
                    ),
                    "motion" => {
                        let level = match value["level"].as_str().unwrap() {
                            "full" => MotionLevel::Full,
                            "fade-only" => MotionLevel::FadeOnly,
                            "off" => MotionLevel::Off,
                            _ => panic!("unknown motion"),
                        };
                        settings.motion = MotionPolicy::new(level, 1.0);
                        command(
                            &mut stream,
                            &replies,
                            next + 100,
                            CommandKind::SetInputSettings(settings.clone().into()),
                        );
                    }
                    "workspace" => command(
                        &mut stream,
                        &replies,
                        next + 100,
                        CommandKind::FocusWorkspace {
                            workspace: value["workspace"].as_u64().unwrap(),
                        },
                    ),
                    "overview" => command(
                        &mut stream,
                        &replies,
                        next + 100,
                        CommandKind::ToggleOverview,
                    ),
                    "capture" => {
                        let filename = value["filename"].as_str().unwrap();
                        assert!(PathBuf::from(filename).is_absolute());
                        let result = bus
                            .call_method(
                                Some("org.gnome.Shell.Screenshot"),
                                "/org/gnome/Shell/Screenshot",
                                Some("org.gnome.Shell.Screenshot"),
                                "Screenshot",
                                &(false, false, filename),
                            )
                            .unwrap();
                        let (success, used): (bool, String) = result.body().deserialize().unwrap();
                        assert!(success);
                        assert_eq!(used, filename);
                    }
                    "quit" => break,
                    _ => panic!("unknown proof action"),
                }
                generation = next;
                let temp = reply.with_extension("tmp");
                std::fs::write(
                    &temp,
                    serde_json::to_vec(
                        &serde_json::json!({"generation":generation,"pid":std::process::id()}),
                    )
                    .unwrap(),
                )
                .unwrap();
                std::fs::rename(temp, &reply).unwrap();
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
