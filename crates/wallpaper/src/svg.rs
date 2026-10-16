//! Bounded transport to the separately packaged GNOME SVG loader. Call only
//! from decoding workers; FontConfig and all filesystem work stay in the
//! helper process. No GdkPixbuf/GTK dependency enters the compositor.
//! Rendered SVGs depend on the native loader and fonts, so callers must not
//! persist them in caches keyed only by the source file (see
//! [`crate::is_raster`]).
use std::{
    io::{Read, Write},
    os::fd::AsRawFd,
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const MAX_INPUT: usize = 64 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(5);

fn helper_path() -> Option<std::path::PathBuf> {
    let executable = std::env::current_exe().ok()?;
    Some(executable.parent()?.join("tuna-wallpaper-svg"))
}

pub fn decode(bytes: &[u8]) -> Option<image::DynamicImage> {
    let output = exchange(bytes, crate::MAX_DECODED_BYTES + 16)?;
    parse_output(output)
}

/// Accept only the exact header plus straight RGBA8 rows of admitted
/// intrinsic dimensions (the same admission as raster decodes).
fn parse_output(mut output: Vec<u8>) -> Option<image::DynamicImage> {
    if output.get(..8)? != b"RSVG0001" {
        return None;
    }
    let w = u32::from_le_bytes(output.get(8..12)?.try_into().ok()?);
    let h = u32::from_le_bytes(output.get(12..16)?.try_into().ok()?);
    let bytes = crate::admit_dimensions(w, h)?;
    if output.len() != 16 + bytes {
        return None;
    }
    output.drain(..16);
    image::RgbaImage::from_raw(w, h, output).map(image::DynamicImage::ImageRgba8)
}

fn nonblocking(fd: &impl AsRawFd) -> Option<()> {
    // SAFETY: live owned pipe descriptor, flags queried before modification.
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return None;
        }
    }
    Some(())
}

fn exchange(bytes: &[u8], maximum: usize) -> Option<Vec<u8>> {
    if bytes.len() > MAX_INPUT {
        return None;
    }
    let mut command = Command::new(helper_path()?);
    command
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            eprintln!("tuna-wallpaper: GNOME SVG helper unavailable: {error}");
            return None;
        }
    };
    // On every exit (including pipe/setup errors), kill and reap the helper.
    // Nonblocking pipes enforce the wall timeout during BOTH input and output,
    // without accumulating extra blocking transport threads behind old jobs.
    let mut reaped = false;
    let result = (|| {
        let mut input = Some(child.stdin.take()?);
        let mut output = child.stdout.take()?;
        nonblocking(input.as_ref()?)?;
        nonblocking(&output)?;
        let started = Instant::now();
        let mut sent = 0;
        let mut received = Vec::new();
        let mut eof = false;
        let mut buffer = [0; 65536];
        loop {
            if started.elapsed() >= TIMEOUT {
                eprintln!("tuna-wallpaper: GNOME SVG helper wall-time bound exceeded");
                return None;
            }
            if let Some(pipe) = input.as_mut() {
                if sent == bytes.len() {
                    input.take();
                } else {
                    match pipe.write(&bytes[sent..]) {
                        Ok(0) => return None,
                        Ok(count) => sent += count,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(_) => return None,
                    }
                }
            }
            // Read available output each iteration so a full pipe cannot
            // deadlock a completed decoder. Check the bound before allocation.
            loop {
                match output.read(&mut buffer) {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(count) => {
                        if received.len().checked_add(count)? > maximum {
                            return None;
                        }
                        received.extend_from_slice(&buffer[..count]);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => return None,
                }
            }
            if let Some(status) = child.try_wait().ok()? {
                reaped = true;
                if !status.success() {
                    eprintln!("tuna-wallpaper: GNOME SVG helper rejected input ({status})");
                    return None;
                }
                if eof && sent == bytes.len() {
                    return Some(received);
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    })();
    // This owned group supplements actual Glycin/bwrap's parent-death chain
    // and PID-namespace cleanup. Never kill unrelated processes by UID.
    // A reaped PID/group number may be reused: only signal while this owned
    // child still reserves its identity. Normal exit uses the native parent-
    // death chain for descendants, whose actual cleanup the receipt checks.
    if !reaped {
        unsafe {
            libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
        }
        let _ = child.kill();
    }
    let _ = child.wait();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn protocol_requires_exact_bounded_intrinsic_rgba() {
        let mut output = b"RSVG0001".to_vec();
        output.extend_from_slice(&2u32.to_le_bytes());
        output.extend_from_slice(&1u32.to_le_bytes());
        output.extend_from_slice(&[25, 87, 200, 255, 200, 10, 20, 128]);
        let image = parse_output(output.clone()).unwrap().to_rgba8();
        assert_eq!(image.dimensions(), (2, 1));
        assert_eq!(image.as_raw(), &output[16..]);
        assert!(parse_output(output[..output.len() - 1].to_vec()).is_none());
        output.push(0);
        assert!(parse_output(output.clone()).is_none());
        output.pop();
        output[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_output(output.clone()).is_none());
        output[8..12].copy_from_slice(&0u32.to_le_bytes());
        assert!(parse_output(output.clone()).is_none());
        // Within the area bound yet beyond the raster geometry cap.
        output[8..12].copy_from_slice(&7681u32.to_le_bytes());
        assert!(parse_output(output).is_none());
    }
}
