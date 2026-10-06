//! API local del daemon: socket Unix con mensajes JSON, una línea por petición y otra por
//! respuesta. La usa la CLI y la usará la UI de bandeja.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum Request {
    Ping,
    Refresh,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    pub message: String,
}

impl Response {
    pub fn ok(message: impl Into<String>) -> Self {
        Self { ok: true, message: message.into() }
    }
    pub fn error(message: impl Into<String>) -> Self {
        Self { ok: false, message: message.into() }
    }
}

/// Envía una petición y espera la respuesta. Falla con `NotFound`/`ConnectionRefused` si
/// no hay daemon, para que el llamador pueda recurrir a otro camino.
pub fn call(socket: &Path, req: &Request) -> io::Result<Response> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer)?;
    serde_json::from_str(&answer).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Lee una petición de una conexión (lado del daemon).
pub fn read_request(stream: &UnixStream) -> io::Result<Request> {
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn write_response(mut stream: &UnixStream, resp: &Response) -> io::Result<()> {
    let mut line = serde_json::to_string(resp)?;
    line.push('\n');
    stream.write_all(line.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formato_json_estable() {
        // La UI dependerá de este formato: cambiarlo debe ser una decisión explícita.
        assert_eq!(serde_json::to_string(&Request::Refresh).unwrap(), r#"{"cmd":"refresh"}"#);
        assert_eq!(serde_json::to_string(&Request::Ping).unwrap(), r#"{"cmd":"ping"}"#);
        assert_eq!(serde_json::to_string(&Response::ok("x")).unwrap(), r#"{"ok":true,"message":"x"}"#);
    }
}
