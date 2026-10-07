//! API local del daemon: mensajes JSON, una línea por petición y otra por respuesta. La usa
//! la CLI y la usará la UI de bandeja. El transporte es un socket Unix en Linux y una named
//! pipe en Windows (`\\.\pipe\cheka`); el protocolo es el mismo.

use std::io::{self, BufRead, BufReader, Read, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::Path;
#[cfg(unix)]
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

/// Manda la petición y lee la respuesta por una conexión ya abierta.
fn exchange(mut conn: impl Read + Write, req: &Request) -> io::Result<Response> {
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    conn.write_all(line.as_bytes())?;
    conn.flush()?;
    let mut answer = String::new();
    BufReader::new(conn).read_line(&mut answer)?;
    serde_json::from_str(&answer).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Envía una petición y espera la respuesta. Falla con `NotFound`/`ConnectionRefused` si
/// no hay daemon, para que el llamador pueda recurrir a otro camino.
#[cfg(unix)]
pub fn call(socket: &Path, req: &Request) -> io::Result<Response> {
    let stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    exchange(&stream, req)
}

/// Windows: `socket` es la ruta de la named pipe. Si todas las instancias están ocupadas
/// (ERROR_PIPE_BUSY), reintenta unos segundos.
#[cfg(windows)]
pub fn call(socket: &Path, req: &Request) -> io::Result<Response> {
    const ERROR_PIPE_BUSY: i32 = 231;
    let mut tries = 0;
    let pipe = loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(socket) {
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && tries < 50 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            other => break other?,
        }
    };
    exchange(&pipe, req)
}

/// Lee una petición de una conexión (lado del daemon).
pub fn read_request(stream: impl Read) -> io::Result<Request> {
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn write_response(mut stream: impl Write, resp: &Response) -> io::Result<()> {
    let mut line = serde_json::to_string(resp)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()
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
