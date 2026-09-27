//! A minimal WebSocket client over a Unix socket, enough to talk JSON-RPC to
//! Codex's shared app-server (text frames, ping/pong, close). Frames we send
//! are masked, as clients must; frames from the server are not.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};

/// Writing half, shared so the reader can answer pings.
#[derive(Clone)]
pub struct Sender(Arc<Mutex<UnixStream>>);

pub struct Receiver {
    stream: UnixStream,
    buf: Vec<u8>,
    sender: Sender,
}

/// Connect and upgrade to WebSocket.
pub fn connect(path: &Path) -> Result<(Sender, Receiver)> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let key = base64(&random_bytes::<16>());
    write!(
        stream,
        "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            bail!("the server closed the connection during the handshake");
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > 16 * 1024 {
            bail!("handshake response too long");
        }
    };
    let status = String::from_utf8_lossy(&buf[..header_end]);
    if !status.starts_with("HTTP/1.1 101") {
        bail!(
            "no WebSocket upgrade: {}",
            status.lines().next().unwrap_or("")
        );
    }
    stream.set_read_timeout(None)?;
    let sender = Sender(Arc::new(Mutex::new(stream.try_clone()?)));
    let receiver = Receiver {
        stream,
        buf: buf[header_end..].to_vec(),
        sender: sender.clone(),
    };
    Ok((sender, receiver))
}

impl Sender {
    pub fn send_text(&self, text: &str) -> Result<()> {
        self.send_frame(0x1, text.as_bytes())
    }

    fn send_frame(&self, opcode: u8, payload: &[u8]) -> Result<()> {
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x80 | opcode);
        let n = payload.len();
        if n < 126 {
            frame.push(0x80 | n as u8);
        } else if n <= u16::MAX as usize {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(n as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(n as u64).to_be_bytes());
        }
        let mask = random_bytes::<4>();
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        let mut s = self.0.lock().map_err(|_| anyhow!("socket lock poisoned"))?;
        s.write_all(&frame)?;
        s.flush()?;
        Ok(())
    }

    pub fn close(&self) {
        let _ = self.send_frame(0x8, &[]);
        if let Ok(s) = self.0.lock() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }
}

impl Receiver {
    fn fill(&mut self, n: usize) -> bool {
        let mut chunk = [0u8; 65536];
        while self.buf.len() < n {
            match self.stream.read(&mut chunk) {
                Ok(0) | Err(_) => return false,
                Ok(k) => self.buf.extend_from_slice(&chunk[..k]),
            }
        }
        true
    }

    /// Next complete text message; `None` when the connection is closed.
    pub fn next_text(&mut self) -> Option<String> {
        let mut message: Vec<u8> = Vec::new();
        loop {
            if !self.fill(2) {
                return None;
            }
            let (b0, b1) = (self.buf[0], self.buf[1]);
            let (fin, opcode, masked) = (b0 & 0x80 != 0, b0 & 0x0f, b1 & 0x80 != 0);
            let mut len = (b1 & 0x7f) as usize;
            let mut off = 2;
            if len == 126 {
                if !self.fill(4) {
                    return None;
                }
                len = u16::from_be_bytes([self.buf[2], self.buf[3]]) as usize;
                off = 4;
            } else if len == 127 {
                if !self.fill(10) {
                    return None;
                }
                len = u64::from_be_bytes(self.buf[2..10].try_into().ok()?) as usize;
                off = 10;
            }
            let mask_len = if masked { 4 } else { 0 };
            if !self.fill(off + mask_len + len) {
                return None;
            }
            let mask: Vec<u8> = self.buf[off..off + mask_len].to_vec();
            let mut payload: Vec<u8> = self.buf[off + mask_len..off + mask_len + len].to_vec();
            if masked {
                for (i, b) in payload.iter_mut().enumerate() {
                    *b ^= mask[i % 4];
                }
            }
            self.buf.drain(..off + mask_len + len);
            match opcode {
                0x8 => return None,
                0x9 => {
                    let _ = self.sender.send_frame(0xA, &payload);
                }
                0xA => {}
                0x0..=0x2 => {
                    message.extend_from_slice(&payload);
                    if fin {
                        return Some(String::from_utf8_lossy(&message).into_owned());
                    }
                }
                _ => {}
            }
        }
    }
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut out);
    }
    out
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_values() {
        assert_eq!(base64(b"hello"), "aGVsbG8=");
        assert_eq!(base64(b"hi"), "aGk=");
        assert_eq!(base64(b"abc"), "YWJj");
    }

    /// A fake server on a socket pair: upgrade, a ping, a fragmented text
    /// message, then close.
    #[test]
    fn frames_round_trip() {
        let dir = std::env::temp_dir().join(format!("peekme-ws-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("s.sock");
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut req = [0u8; 1024];
            let _ = s.read(&mut req).unwrap();
            s.write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n")
                .unwrap();
            // ping, then "hel" + "lo" as two fragments, then close
            s.write_all(&[0x89, 0x01, b'x']).unwrap();
            s.write_all(&[0x01, 0x03, b'h', b'e', b'l']).unwrap();
            s.write_all(&[0x80, 0x02, b'l', b'o']).unwrap();
            // read the client's masked text frame
            let mut hdr = [0u8; 2];
            s.read_exact(&mut hdr).unwrap();
            // first frame from the client may be the pong; skip until a text frame
            let mut frames = Vec::new();
            loop {
                let len = (hdr[1] & 0x7f) as usize;
                let mut mask = [0u8; 4];
                s.read_exact(&mut mask).unwrap();
                let mut p = vec![0u8; len];
                s.read_exact(&mut p).unwrap();
                for (i, b) in p.iter_mut().enumerate() {
                    *b ^= mask[i % 4];
                }
                frames.push((hdr[0] & 0x0f, p));
                if hdr[0] & 0x0f == 0x1 {
                    break;
                }
                s.read_exact(&mut hdr).unwrap();
            }
            s.write_all(&[0x88, 0x00]).unwrap();
            frames
        });
        let (tx, mut rx) = connect(&path).unwrap();
        assert_eq!(rx.next_text().as_deref(), Some("hello"));
        tx.send_text("{\"id\":1}").unwrap();
        assert_eq!(rx.next_text(), None);
        let frames = server.join().unwrap();
        assert_eq!(frames[0], (0xA, b"x".to_vec()), "ping answered with pong");
        assert_eq!(frames.last().unwrap(), &(0x1, b"{\"id\":1}".to_vec()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
