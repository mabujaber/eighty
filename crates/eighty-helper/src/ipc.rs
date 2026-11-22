//! Unix-socket IPC server: one connection at a time, peer uid from the kernel.
use crate::engine::Event;
use eighty_core::proto::{read_msg, write_msg, Request, Response};
use std::io::{self, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

const IO_TIMEOUT: Duration = Duration::from_secs(2);
const READ_DEADLINE: Duration = Duration::from_secs(3);
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

pub fn bind(path: &Path) -> io::Result<UnixListener> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let l = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
    Ok(l)
}

/// Reads from a stream, failing with TimedOut once a total deadline has passed.
struct DeadlineReader<'a> {
    stream: &'a UnixStream,
    deadline: Instant,
}

impl Read for DeadlineReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(left))?;
        self.stream.read(buf)
    }
}

fn peer_uid(s: &UnixStream) -> io::Result<u32> {
    let (mut uid, mut gid): (libc::uid_t, libc::gid_t) = (0, 0);
    // SAFETY: valid fd and valid out-pointers for the duration of the call.
    if unsafe { libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}

/// Returns false when the engine is gone and serving should stop.
fn handle(stream: UnixStream, tx: &Sender<Event>, reply_timeout: Duration) -> bool {
    let deadline = Instant::now() + READ_DEADLINE;
    let Ok(uid) = peer_uid(&stream) else { return true };
    if stream.set_read_timeout(Some(IO_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(IO_TIMEOUT)).is_err()
    {
        return true;
    }
    let mut w = &stream;
    let req: Request = match read_msg(&mut BufReader::new(DeadlineReader { stream: &stream, deadline })) {
        Ok(r) => r,
        Err(e) if e.kind() == io::ErrorKind::InvalidData => {
            let _ = write_msg(&mut w, &Response::Error(format!("bad request: {e}")));
            return true;
        }
        Err(_) => return true,
    };
    let (rtx, rrx) = mpsc::channel();
    if tx.send(Event::Request { uid, req, reply: rtx }).is_err() {
        return false;
    }
    let resp = rrx
        .recv_timeout(reply_timeout)
        .unwrap_or_else(|_| Response::Error("helper busy".to_string()));
    let _ = write_msg(&mut w, &resp);
    true
}

pub fn serve(l: UnixListener, tx: Sender<Event>) {
    serve_with(l, tx, REPLY_TIMEOUT)
}

fn serve_with(l: UnixListener, tx: Sender<Event>, reply_timeout: Duration) {
    for stream in l.incoming() {
        let Ok(stream) = stream else { continue };
        if !handle(stream, &tx, reply_timeout) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eighty_core::proto::MAX_LINE;
    use std::io::Write;
    use std::sync::mpsc::Receiver;

    fn spawn_echo_engine(rx: Receiver<Event>) {
        std::thread::spawn(move || {
            while let Ok(ev) = rx.recv() {
                if let Event::Request { uid, req: Request::Status, reply } = ev {
                    let _ = reply.send(Response::Error(format!("uid {uid}")));
                }
            }
        });
    }

    fn setup() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        let (tx, rx) = mpsc::channel();
        let l = bind(&path).unwrap();
        std::thread::spawn(move || serve(l, tx));
        spawn_echo_engine(rx);
        (dir, path)
    }

    #[test]
    fn request_reaches_engine_with_peer_uid() {
        let (_d, path) = setup();
        let mut c = UnixStream::connect(&path).unwrap();
        write_msg(&mut c, &Request::Status).unwrap();
        let r: Response = read_msg(&mut BufReader::new(c)).unwrap();
        assert_eq!(r, Response::Error(format!("uid {}", unsafe { libc::getuid() })));
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o666);
    }

    #[test]
    fn garbage_and_silent_clients_do_not_block_server() {
        let (_d, path) = setup();
        let silent = UnixStream::connect(&path).unwrap();
        let mut bad = UnixStream::connect(&path).unwrap();
        bad.write_all(b"{nope\n").unwrap();
        let mut long = UnixStream::connect(&path).unwrap();
        let _ = long.write_all(&vec![b'a'; MAX_LINE + 1]);
        let t = std::time::Instant::now();
        let mut ok = UnixStream::connect(&path).unwrap();
        write_msg(&mut ok, &Request::Status).unwrap();
        let _: Response = read_msg(&mut BufReader::new(ok)).unwrap();
        assert!(t.elapsed() < Duration::from_secs(8));
        let r: Response = read_msg(&mut BufReader::new(bad)).unwrap();
        assert!(matches!(r, Response::Error(m) if m.starts_with("bad request")));
        drop(silent);
    }

    #[test]
    fn slow_drip_client_is_dropped_by_total_deadline() {
        let (_d, path) = setup();
        let mut drip = UnixStream::connect(&path).unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s2 = stop.clone();
        let h = std::thread::spawn(move || {
            while !s2.load(std::sync::atomic::Ordering::Relaxed) {
                if drip.write_all(b"a").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        });
        let t = Instant::now();
        let mut ok = UnixStream::connect(&path).unwrap();
        write_msg(&mut ok, &Request::Status).unwrap();
        let _: Response = read_msg(&mut BufReader::new(ok)).unwrap();
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        h.join().unwrap();
    }

    #[test]
    fn replies_busy_when_engine_does_not_answer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        let (tx, rx) = mpsc::channel();
        let l = bind(&path).unwrap();
        std::thread::spawn(move || serve_with(l, tx, Duration::from_millis(200)));
        let _keep = std::thread::spawn(move || {
            let held: Vec<Event> = rx.iter().collect();
            drop(held);
        });
        let mut c = UnixStream::connect(&path).unwrap();
        write_msg(&mut c, &Request::Status).unwrap();
        let r: Response = read_msg(&mut BufReader::new(c)).unwrap();
        assert_eq!(r, Response::Error("helper busy".to_string()));
    }

    #[test]
    fn serve_returns_when_engine_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        let (tx, rx) = mpsc::channel();
        drop(rx);
        let l = bind(&path).unwrap();
        let h = std::thread::spawn(move || serve(l, tx));
        let mut c = UnixStream::connect(&path).unwrap();
        write_msg(&mut c, &Request::Status).unwrap();
        h.join().unwrap();
    }

    #[test]
    fn bind_replaces_stale_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        std::fs::write(&path, b"stale").unwrap();
        assert!(bind(&path).is_ok());
    }
}
