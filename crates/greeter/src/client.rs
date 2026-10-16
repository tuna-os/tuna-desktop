//! Greetd JSON-IPC client: blocking conversation driver over a Unix
//! stream using the adopted `greetd_ipc` crate's sync codec. One
//! method per protocol step; the live GTK controller drives
//! the prompt loop against [`crate::model::GreeterModel`].

use std::os::unix::net::UnixStream;

use greetd_ipc::{
    codec::{Error as CodecError, SyncCodec},
    Request, Response,
};

/// A connected conversation with the login daemon.
pub struct GreeterClient {
    stream: UnixStream,
}

impl GreeterClient {
    /// Connect to the daemon at `socket_path` (`$GREETD_SOCK` live).
    pub fn connect(socket_path: &str) -> std::io::Result<Self> {
        Ok(Self {
            stream: {
                let stream = UnixStream::connect(socket_path)?;
                stream.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
                stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
                stream
            },
        })
    }

    /// Wrap an already-connected stream (tests, harnesses).
    pub fn from_stream(stream: UnixStream) -> Self {
        Self { stream }
    }

    /// Open a login attempt for `user`; returns the daemon's first
    /// response (auth message, success, or failure).
    pub fn create_session(&mut self, user: &str) -> Result<Response, CodecError> {
        Request::CreateSession {
            username: user.to_string(),
        }
        .write_to(&mut self.stream)?;
        self.read_response()
    }

    /// Answer the last auth message (`None` acknowledges info/error
    /// messages that carry no question).
    pub fn answer(&mut self, text: Option<String>) -> Result<Response, CodecError> {
        Request::PostAuthMessageResponse { response: text }.write_to(&mut self.stream)?;
        self.read_response()
    }

    fn read_response(&mut self) -> Result<Response, CodecError> {
        use std::io::Read;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        fn read_until(
            stream: &mut UnixStream,
            bytes: &mut [u8],
            deadline: std::time::Instant,
        ) -> Result<(), CodecError> {
            let mut offset = 0;
            while offset < bytes.len() {
                let remaining = deadline
                    .checked_duration_since(std::time::Instant::now())
                    .ok_or_else(|| CodecError::Io("greetd response timed out".into()))?;
                stream.set_read_timeout(Some(remaining))?;
                match stream.read(&mut bytes[offset..]) {
                    Ok(0) => return Err(CodecError::Eof),
                    Ok(count) => offset += count,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(())
        }
        let mut header = [0; 4];
        read_until(&mut self.stream, &mut header, deadline)?;
        let size = u32::from_ne_bytes(header) as usize;
        if size > 256 * 1024 {
            return Err(CodecError::Io("greetd response exceeds limit".into()));
        }
        let mut body = vec![0; size];
        read_until(&mut self.stream, &mut body, deadline)?;
        Response::read_from(&mut std::io::Cursor::new(header).chain(std::io::Cursor::new(body)))
    }

    /// Keep a shutdown handle so closing the UI interrupts a blocking exchange.
    pub fn shutdown_handle(&self) -> std::io::Result<UnixStream> {
        self.stream.try_clone()
    }

    /// Every greetd request has a response, including StartSession and Cancel.
    /// The session starts only after Success AND the greeter exits.
    pub fn start_session_response(
        &mut self,
        cmd: Vec<String>,
        env: Vec<String>,
    ) -> Result<Response, CodecError> {
        Request::StartSession { cmd, env }.write_to(&mut self.stream)?;
        self.read_response()
    }

    pub fn cancel_response(&mut self) -> Result<Response, CodecError> {
        Request::CancelSession.write_to(&mut self.stream)?;
        self.read_response()
    }

    pub fn start_session(&mut self, cmd: Vec<String>, env: Vec<String>) -> Result<(), CodecError> {
        require_success(self.start_session_response(cmd, env)?)
    }

    pub fn cancel(&mut self) -> Result<(), CodecError> {
        require_success(self.cancel_response()?)
    }
}

fn require_success(response: Response) -> Result<(), CodecError> {
    match response {
        Response::Success => Ok(()),
        // Never leak daemon authentication details through an error string.
        _ => Err(CodecError::Io("greetd rejected request".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use greetd_ipc::AuthMessageType;
    use std::thread;

    /// Scripted peer: answers CreateSession with one secret prompt.
    fn peer(mut stream: UnixStream) {
        let req = Request::read_from(&mut stream).unwrap();
        assert!(matches!(req, Request::CreateSession { .. }));
        Response::AuthMessage {
            auth_message_type: AuthMessageType::Secret,
            auth_message: "Password:".to_string(),
        }
        .write_to(&mut stream)
        .unwrap();
    }

    #[test]
    fn create_session_roundtrip() {
        let (a, b) = UnixStream::pair().unwrap();
        let handle = thread::spawn(move || peer(a));
        let mut client = GreeterClient::from_stream(b);
        let resp = client.create_session("ada").unwrap();
        assert!(matches!(
            resp,
            Response::AuthMessage {
                auth_message_type: AuthMessageType::Secret,
                ..
            }
        ));
        handle.join().unwrap();
    }
}

#[cfg(test)]
mod reply_tests {
    use super::*;
    use std::{io::Write, thread};
    #[test]
    fn cancel_reply_is_consumed_before_next_create() {
        let (a, mut b) = UnixStream::pair().unwrap();
        let peer = thread::spawn(move || {
            assert!(matches!(
                Request::read_from(&mut b).unwrap(),
                Request::CancelSession
            ));
            Response::Success.write_to(&mut b).unwrap();
            assert!(matches!(
                Request::read_from(&mut b).unwrap(),
                Request::CreateSession { .. }
            ));
            Response::AuthMessage {
                auth_message_type: greetd_ipc::AuthMessageType::Visible,
                auth_message: "Next prompt".into(),
            }
            .write_to(&mut b)
            .unwrap();
        });
        let mut client = GreeterClient::from_stream(a);
        client.cancel().unwrap();
        assert!(matches!(
            client.create_session("ada").unwrap(),
            Response::AuthMessage { .. }
        ));
        peer.join().unwrap();
    }
    #[test]
    fn rejected_start_is_not_reported_as_success() {
        let (a, mut b) = UnixStream::pair().unwrap();
        let peer = thread::spawn(move || {
            assert!(matches!(
                Request::read_from(&mut b).unwrap(),
                Request::StartSession { .. }
            ));
            Response::Error {
                error_type: greetd_ipc::ErrorType::Error,
                description: "private".into(),
            }
            .write_to(&mut b)
            .unwrap();
        });
        let error = GreeterClient::from_stream(a)
            .start_session(vec!["tuna-session".into()], vec![])
            .unwrap_err();
        assert!(!error.to_string().contains("private"));
        peer.join().unwrap();
    }
    #[test]
    fn oversized_frame_is_rejected_before_body_allocation_or_wait() {
        let (a, mut b) = UnixStream::pair().unwrap();
        let peer = thread::spawn(move || {
            Request::read_from(&mut b).unwrap();
            b.write_all(&u32::MAX.to_ne_bytes()).unwrap();
        });
        assert!(GreeterClient::from_stream(a).create_session("ada").is_err());
        peer.join().unwrap();
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;
    use std::{net::Shutdown, sync::mpsc, thread, time::Duration};
    #[test]
    fn shutdown_interrupts_inflight_reply_without_waiting_for_daemon() {
        let (a, mut b) = UnixStream::pair().unwrap();
        let mut client = GreeterClient::from_stream(a);
        let interrupt = client.shutdown_handle().unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            done_tx.send(client.create_session("ada").is_err()).unwrap();
        });
        assert!(matches!(
            Request::read_from(&mut b).unwrap(),
            Request::CreateSession { .. }
        ));
        // Keep the peer open and silent, as a blocked PAM conversation could.
        interrupt.shutdown(Shutdown::Both).unwrap();
        assert!(done_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        worker.join().unwrap();
    }
}
