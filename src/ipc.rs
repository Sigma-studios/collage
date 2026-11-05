//! Tiny line-based protocol over a unix socket between the CLI/popup and the
//! daemon. One request line per connection, one reply line back.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Save the current primary selection (highlighted text) as a note.
    Note,
    /// Put a stored entry back on the clipboard.
    Copy(i64),
    /// Open the history/notes popup.
    Popup,
    Ping,
    Quit,
}

impl Request {
    pub fn encode(&self) -> String {
        match self {
            Request::Note => "note".into(),
            Request::Copy(id) => format!("copy {id}"),
            Request::Popup => "popup".into(),
            Request::Ping => "ping".into(),
            Request::Quit => "quit".into(),
        }
    }

    pub fn decode(line: &str) -> Option<Self> {
        let mut parts = line.split_whitespace();
        Some(match (parts.next()?, parts.next()) {
            ("note", None) => Request::Note,
            ("copy", Some(id)) => Request::Copy(id.parse().ok()?),
            ("popup", None) => Request::Popup,
            ("ping", None) => Request::Ping,
            ("quit", None) => Request::Quit,
            _ => return None,
        })
    }
}

pub fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

pub fn socket_path() -> PathBuf {
    runtime_dir().join("collage.sock")
}

/// Send a request to the running daemon and return its reply.
pub fn send(req: &Request) -> Result<String> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path).with_context(|| {
        format!(
            "can't reach the collage daemon at {} — is `collage daemon` running?",
            path.display()
        )
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.write_all(req.encode().as_bytes())?;
    stream.write_all(b"\n")?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    let reply = reply.trim_end().to_string();
    match reply.strip_prefix("err ") {
        Some(msg) => bail!("{msg}"),
        None => Ok(reply),
    }
}

/// Bind the daemon socket, refusing to start if another daemon answers.
pub fn bind() -> Result<UnixListener> {
    let path = socket_path();
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            bail!("collage daemon is already running ({})", path.display());
        }
        std::fs::remove_file(&path).ok();
    }
    let listener =
        UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// Read one request from an accepted connection.
pub fn read_request(stream: &UnixStream) -> Result<Option<Request>> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Ok(Request::decode(line.trim()))
}

pub fn reply(mut stream: &UnixStream, result: Result<String>) {
    let line = match result {
        Ok(msg) => format!("ok {msg}\n"),
        Err(err) => format!("err {err}\n"),
    };
    let _ = stream.write_all(line.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for req in [
            Request::Note,
            Request::Copy(42),
            Request::Popup,
            Request::Ping,
            Request::Quit,
        ] {
            assert_eq!(Request::decode(&req.encode()), Some(req));
        }
        assert_eq!(Request::decode("copy nope"), None);
        assert_eq!(Request::decode("bogus"), None);
    }
}
