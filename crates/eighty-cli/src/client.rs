//! Blocking client for the helper's Unix socket.
use eighty_core::proto::{read_msg, write_msg, Request, Response};
use std::io::{self, BufReader};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

#[derive(thiserror::Error, Debug)]
pub enum ClientError {
    #[error("the eighty helper isn't running. Install it with `sudo eighty install`.")]
    NotRunning,
    #[error("can't talk to the eighty helper: {0}")]
    Io(io::Error),
    #[error("the eighty helper sent an invalid reply: {0}")]
    Protocol(String),
}

impl From<io::Error> for ClientError {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::InvalidData => ClientError::Protocol(e.to_string()),
            _ => ClientError::Io(e),
        }
    }
}

pub fn call(path: &Path, req: &Request) -> Result<Response, ClientError> {
    let mut stream = UnixStream::connect(path).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => ClientError::NotRunning,
        _ => ClientError::Io(e),
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    write_msg(&mut stream, req)?;
    Ok(read_msg(&mut BufReader::new(stream))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_running_when_no_socket() {
        let d = tempfile::tempdir().unwrap();
        assert!(matches!(
            call(&d.path().join("none.sock"), &Request::Status),
            Err(ClientError::NotRunning)
        ));
    }
}
