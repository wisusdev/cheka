//! Servidor DNS mínimo para `*.test` (Windows). En Linux este papel lo cumple dnsmasq; en
//! Windows una regla NRPT (`.test → 127.0.0.1`) manda aquí solo las consultas de `.test`,
//! así que cualquier subdominio (`tienda.blog.test`) resuelve sin tocar el archivo `hosts`.
//!
//! Responde A = 127.0.0.1 para el TLD y todo lo que cuelga de él, sin registros AAAA (así
//! los navegadores usan IPv4) y REFUSED para cualquier otro dominio.

use std::io;
use std::net::UdpSocket;

const TYPE_A: u16 = 1;
const TYPE_ANY: u16 = 255;
const RCODE_REFUSED: u8 = 5;
const RCODE_FORMERR: u8 = 1;
const TTL_SECS: u32 = 60;

/// Nombre de la pregunta (en minúsculas, sin el punto final) y dónde termina.
fn question_name(msg: &[u8]) -> Option<(String, usize)> {
    let mut i = 12;
    let mut labels = Vec::new();
    loop {
        let len = *msg.get(i)? as usize;
        i += 1;
        if len == 0 {
            break;
        }
        if len > 63 {
            return None; // punteros o etiquetas inválidas: no van en una pregunta
        }
        labels.push(String::from_utf8_lossy(msg.get(i..i + len)?).to_ascii_lowercase());
        i += len;
    }
    Some((labels.join("."), i))
}

/// Respuesta a una consulta, o `None` si no se puede ni leer el encabezado.
pub fn answer(query: &[u8], tld: &str) -> Option<Vec<u8>> {
    if query.len() < 12 {
        return None;
    }
    let rd = query[2] & 0x01;
    let header = |rcode: u8, qd: u16, an: u16| -> Vec<u8> {
        let mut h = vec![query[0], query[1], 0x84 | rd, 0x80 | rcode]; // QR, AA, RD; RA
        h.extend_from_slice(&qd.to_be_bytes());
        h.extend_from_slice(&an.to_be_bytes());
        h.extend_from_slice(&[0, 0, 0, 0]);
        h
    };
    let qdcount = u16::from_be_bytes([query[4], query[5]]);
    let parsed = if qdcount == 1 { question_name(query) } else { None };
    let Some((name, end)) = parsed.filter(|(_, end)| query.len() >= end + 4) else {
        return Some(header(RCODE_FORMERR, 0, 0));
    };
    let question = &query[12..end + 4];
    let qtype = u16::from_be_bytes([query[end], query[end + 1]]);
    let ours = name == tld || name.ends_with(&format!(".{tld}"));
    if !ours {
        let mut r = header(RCODE_REFUSED, 1, 0);
        r.extend_from_slice(question);
        return Some(r);
    }
    let with_a = qtype == TYPE_A || qtype == TYPE_ANY;
    let mut r = header(0, 1, u16::from(with_a));
    r.extend_from_slice(question);
    if with_a {
        r.extend_from_slice(&[0xC0, 0x0C]); // el nombre de la pregunta (offset 12)
        r.extend_from_slice(&TYPE_A.to_be_bytes());
        r.extend_from_slice(&1u16.to_be_bytes()); // IN
        r.extend_from_slice(&TTL_SECS.to_be_bytes());
        r.extend_from_slice(&4u16.to_be_bytes());
        r.extend_from_slice(&[127, 0, 0, 1]);
    }
    Some(r)
}

/// Atiende consultas en `addr` (p. ej. "127.0.0.1:53") en un hilo propio.
pub fn spawn(addr: &str, tld: &str) -> io::Result<()> {
    let socket = UdpSocket::bind(addr)?;
    let tld = tld.to_ascii_lowercase();
    std::thread::spawn(move || {
        let mut buf = [0u8; 512];
        loop {
            let Ok((n, from)) = socket.recv_from(&mut buf) else { continue };
            if let Some(resp) = answer(&buf[..n], &tld) {
                let _ = socket.send_to(&resp, from);
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(name: &str, qtype: u16) -> Vec<u8> {
        let mut q = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in name.split('.') {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&1u16.to_be_bytes());
        q
    }

    #[test]
    fn responde_cualquier_subdominio_de_test() {
        for name in ["blog.test", "Tienda.Blog.TEST", "a.b.c.test", "test"] {
            let r = answer(&query(name, TYPE_A), "test").unwrap();
            assert_eq!(&r[..2], &[0x12, 0x34]);
            assert_eq!(r[3] & 0x0F, 0, "{name}");
            assert_eq!(u16::from_be_bytes([r[6], r[7]]), 1, "{name}");
            assert_eq!(&r[r.len() - 4..], &[127, 0, 0, 1]);
        }
    }

    #[test]
    fn servidor_por_udp() {
        // Puerto libre elegido por el sistema.
        let port = UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        spawn(&format!("127.0.0.1:{port}"), "test").unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
        client.send_to(&query("sub.mi-sitio.test", TYPE_A), ("127.0.0.1", port)).unwrap();
        let mut buf = [0u8; 512];
        let n = client.recv(&mut buf).unwrap();
        assert_eq!(&buf[n - 4..n], &[127, 0, 0, 1]);
    }

    #[test]
    fn sin_aaaa_y_rechaza_otros_dominios() {
        let r = answer(&query("blog.test", 28), "test").unwrap();
        assert_eq!((r[3] & 0x0F, u16::from_be_bytes([r[6], r[7]])), (0, 0));
        let r = answer(&query("google.com", TYPE_A), "test").unwrap();
        assert_eq!(r[3] & 0x0F, RCODE_REFUSED);
        let r = answer(&query("contest", TYPE_A), "test").unwrap();
        assert_eq!(r[3] & 0x0F, RCODE_REFUSED);
        assert_eq!(answer(&[1, 2, 3], "test"), None);
    }
}
