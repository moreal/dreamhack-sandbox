//! Streaming and whole-file SHA-256 helpers (DESIGN.md §3.6).

use std::io::{Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::Result;

/// A `Write` adapter that hashes and counts every byte passed through it
/// before forwarding it to the wrapped writer.
pub struct Sha256Writer<W: Write> {
    inner: W,
    hasher: Sha256,
    len: u64,
}

impl<W: Write> Sha256Writer<W> {
    pub fn new(inner: W) -> Self {
        Sha256Writer {
            inner,
            hasher: Sha256::new(),
            len: 0,
        }
    }

    /// Consumes the writer, returning the wrapped writer, the lowercase hex
    /// SHA-256 digest, and the total number of bytes written.
    pub fn finish(self) -> (W, String, u64) {
        (self.inner, hex::encode(self.hasher.finalize()), self.len)
    }
}

impl<W: Write> Write for Sha256Writer<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.len += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Hashes a whole file on disk, returning `(hex digest, byte length)`.
pub fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut len = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        len += n as u64;
    }
    Ok((hex::encode(hasher.finalize()), len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_writer_matches_sha2() {
        let data = b"hello wargame\n";
        let mut writer = Sha256Writer::new(Vec::new());
        writer.write_all(data).unwrap();
        let (buf, digest, len) = writer.finish();
        assert_eq!(buf, data);
        assert_eq!(len, data.len() as u64);

        let mut expected = Sha256::new();
        expected.update(data);
        assert_eq!(digest, hex::encode(expected.finalize()));
    }

    #[test]
    fn sha256_file_matches_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        std::fs::write(&path, b"PK\x03\x04fake").unwrap();
        let (digest, len) = sha256_file(&path).unwrap();
        assert_eq!(len, 8);

        let mut expected = Sha256::new();
        expected.update(b"PK\x03\x04fake");
        assert_eq!(digest, hex::encode(expected.finalize()));
    }
}
