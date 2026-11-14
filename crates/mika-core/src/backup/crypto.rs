//! Password, keys and the encrypted body of a backup. Everything from `ring`.
//!
//! The body is cut into chunks, each sealed with AES-256-GCM. The chunk
//! number and a "last chunk" flag are part of the nonce and the hash of the
//! file header is the associated data, so reordered chunks, a cut file and an
//! edited header do not open.

use std::io::{self, Read, Write};
use std::num::NonZeroU32;

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::rand::SecureRandom;
use ring::{hkdf, hmac, pbkdf2};

use super::{BackupError, MAX_PASSWORD, MIN_PASSWORD};

/// Random bytes of the header nonce; the rest of the nonce is the chunk
/// number and the last flag.
pub(crate) const NONCE_PREFIX: usize = 7;
pub(crate) const SALT_LEN: usize = 16;
pub(crate) const CHECK_LEN: usize = 32;
/// Bytes the authentication tag adds to every chunk.
pub(crate) const TAG_LEN: usize = 16;

const ENC_INFO: &[u8] = b"mika-backup/v1/enc";
const CHECK_INFO: &[u8] = b"mika-backup/v1/check";

/// A password for a backup: 10 to 256 characters, whatever they are.
pub fn check_password(password: &str) -> Result<(), BackupError> {
    let chars = password.chars().count();
    if chars < MIN_PASSWORD {
        Err(BackupError::PasswordShort(MIN_PASSWORD))
    } else if chars > MAX_PASSWORD {
        Err(BackupError::PasswordLong(MAX_PASSWORD))
    } else {
        Ok(())
    }
}

/// The two keys made from a password: one seals the body, one proves the
/// password before anything is opened.
#[derive(Clone)]
pub(crate) struct Keys {
    enc: LessSafeKey,
    check: hmac::Key,
}

impl Keys {
    /// Slow on purpose (PBKDF2). The password goes in exactly as typed.
    pub(crate) fn derive(password: &str, salt: &[u8], iterations: NonZeroU32) -> Keys {
        let mut master = [0u8; 32];
        pbkdf2::derive(
            pbkdf2::PBKDF2_HMAC_SHA256,
            iterations,
            salt,
            password.as_bytes(),
            &mut master,
        );
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(&master);
        // Best effort: the compiler may keep a copy elsewhere.
        master.iter_mut().for_each(|b| *b = 0);
        let enc = prk
            .expand(&[ENC_INFO], &AES_256_GCM)
            .map(UnboundKey::from)
            .map(LessSafeKey::new)
            .expect("HKDF gives 32 bytes");
        let check = prk
            .expand(&[CHECK_INFO], hmac::HMAC_SHA256)
            .map(hmac::Key::from)
            .expect("HKDF gives 32 bytes");
        Keys { enc, check }
    }

    /// The value stored in the header to recognise the password later.
    pub(crate) fn check_value(&self) -> [u8; CHECK_LEN] {
        let mut out = [0u8; CHECK_LEN];
        out.copy_from_slice(hmac::sign(&self.check, CHECK_INFO).as_ref());
        out
    }

    /// Whether these keys come from the password the file was made with.
    pub(crate) fn matches(&self, check: &[u8]) -> bool {
        hmac::verify(&self.check, CHECK_INFO, check).is_ok()
    }
}

/// Fresh random bytes from the system.
pub(crate) fn random<const N: usize>() -> Result<[u8; N], BackupError> {
    let mut out = [0u8; N];
    ring::rand::SystemRandom::new()
        .fill(&mut out)
        .map_err(|_| BackupError::Io(io::Error::other("random generator failed")))?;
    Ok(out)
}

/// What every chunk of one file shares.
#[derive(Clone)]
pub(crate) struct Sealing {
    pub keys: Keys,
    pub nonce: [u8; NONCE_PREFIX],
    /// SHA-256 of the file prefix: magic, format, header.
    pub aad: [u8; 32],
    /// Plaintext bytes per chunk.
    pub chunk: usize,
}

impl Sealing {
    fn nonce(&self, index: u32, last: bool) -> Nonce {
        let mut n = [0u8; NONCE_LEN];
        n[..NONCE_PREFIX].copy_from_slice(&self.nonce);
        n[NONCE_PREFIX..NONCE_PREFIX + 4].copy_from_slice(&index.to_be_bytes());
        n[NONCE_LEN - 1] = u8::from(last);
        Nonce::assume_unique_for_key(n)
    }
}

fn damaged(what: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

/// Writes the encrypted body: `u32 LE length` and the sealed chunk, again and
/// again. A full buffer is sealed only when more data follows, so the last
/// chunk is always the one `finish` seals.
pub(crate) struct ChunkWriter<W: Write> {
    out: W,
    sealing: Sealing,
    buf: Vec<u8>,
    index: u32,
}

impl<W: Write> ChunkWriter<W> {
    pub(crate) fn new(out: W, sealing: Sealing) -> Self {
        let buf = Vec::with_capacity(sealing.chunk + TAG_LEN);
        Self {
            out,
            sealing,
            buf,
            index: 0,
        }
    }

    fn seal(&mut self, last: bool) -> io::Result<()> {
        let nonce = self.sealing.nonce(self.index, last);
        self.sealing
            .keys
            .enc
            .seal_in_place_append_tag(nonce, Aad::from(self.sealing.aad), &mut self.buf)
            .map_err(|_| io::Error::other("cannot seal a backup chunk"))?;
        self.out.write_all(&(self.buf.len() as u32).to_le_bytes())?;
        self.out.write_all(&self.buf)?;
        self.buf.clear();
        self.index = self
            .index
            .checked_add(1)
            .ok_or_else(|| io::Error::other("too many backup chunks"))?;
        Ok(())
    }

    /// Seals what is left, possibly nothing, as the last chunk.
    pub(crate) fn finish(mut self) -> io::Result<W> {
        self.seal(true)?;
        Ok(self.out)
    }
}

impl<W: Write> Write for ChunkWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        if self.buf.len() == self.sealing.chunk {
            self.seal(false)?;
        }
        let take = data.len().min(self.sealing.chunk - self.buf.len());
        self.buf.extend_from_slice(&data[..take]);
        Ok(take)
    }

    /// Chunks are sealed whole: nothing can be pushed out early.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Reads the encrypted body and gives the plaintext. A chunk is the last one
/// exactly when the input ends after it; anything that does not open is an
/// `InvalidData` error.
pub(crate) struct ChunkReader<R: Read> {
    input: R,
    sealing: Sealing,
    /// The open chunk and how much of it was handed out.
    plain: Vec<u8>,
    pos: usize,
    index: u32,
    /// Length of the chunk that follows, read ahead to learn whether the
    /// current one is the last.
    next_len: Option<usize>,
    started: bool,
    done: bool,
}

impl<R: Read> ChunkReader<R> {
    pub(crate) fn new(input: R, sealing: Sealing) -> Self {
        Self {
            input,
            sealing,
            plain: Vec::new(),
            pos: 0,
            index: 0,
            next_len: None,
            started: false,
            done: false,
        }
    }

    /// The length of the next chunk, or `None` when the input has ended.
    fn read_len(&mut self) -> io::Result<Option<usize>> {
        let mut raw = [0u8; 4];
        let mut got = 0;
        while got < raw.len() {
            match self.input.read(&mut raw[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        match got {
            0 => Ok(None),
            4 => {
                let len = u32::from_le_bytes(raw) as usize;
                if (TAG_LEN..=self.sealing.chunk + TAG_LEN).contains(&len) {
                    Ok(Some(len))
                } else {
                    Err(damaged("backup chunk has an impossible length"))
                }
            }
            _ => Err(damaged("backup ends inside a chunk length")),
        }
    }

    /// Opens the next chunk into `plain`. `false` when there is none.
    fn next_chunk(&mut self) -> io::Result<bool> {
        if self.done {
            return Ok(false);
        }
        if !self.started {
            self.started = true;
            self.next_len = self.read_len()?;
            if self.next_len.is_none() {
                return Err(damaged("backup has no encrypted chunk"));
            }
        }
        let Some(len) = self.next_len else {
            self.done = true;
            return Ok(false);
        };
        self.plain.resize(len, 0);
        self.input.read_exact(&mut self.plain).map_err(|e| {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                damaged("backup ends inside a chunk")
            } else {
                e
            }
        })?;
        self.next_len = self.read_len()?;
        let last = self.next_len.is_none();
        let nonce = self.sealing.nonce(self.index, last);
        let opened = self
            .sealing
            .keys
            .enc
            .open_in_place(nonce, Aad::from(self.sealing.aad), &mut self.plain)
            .map_err(|_| damaged("backup chunk does not open"))?
            .len();
        self.plain.truncate(opened);
        self.pos = 0;
        self.index = self
            .index
            .checked_add(1)
            .ok_or_else(|| damaged("too many backup chunks"))?;
        Ok(true)
    }
}

impl<R: Read> Read for ChunkReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        // An empty chunk in the middle is legal for the format: skip it.
        while self.pos == self.plain.len() {
            if !self.next_chunk()? {
                return Ok(0);
            }
        }
        let take = out.len().min(self.plain.len() - self.pos);
        out[..take].copy_from_slice(&self.plain[self.pos..self.pos + take]);
        self.pos += take;
        Ok(take)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::backup::{CHUNK, KDF_ITERATIONS_MIN};

    /// Keys from the lowest iteration count a file may name: fast enough for tests.
    pub(crate) fn test_keys(password: &str, salt: &[u8]) -> Keys {
        Keys::derive(password, salt, NonZeroU32::new(KDF_ITERATIONS_MIN).unwrap())
    }

    fn sealing(chunk: usize) -> Sealing {
        // Sealing does not depend on how the key was made: a cheap key keeps
        // the stream tests quick.
        let keys = Keys::derive("stream tests", b"0123456789abcdef", NonZeroU32::MIN);
        Sealing {
            keys,
            nonce: [7; NONCE_PREFIX],
            aad: [9; 32],
            chunk,
        }
    }

    fn seal_all(s: &Sealing, data: &[u8]) -> Vec<u8> {
        let mut w = ChunkWriter::new(Vec::new(), s.clone());
        // Odd pieces, so chunk borders fall inside a write.
        for piece in data.chunks(1000) {
            w.write_all(piece).unwrap();
        }
        w.finish().unwrap()
    }

    fn open_all(s: &Sealing, sealed: &[u8]) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        ChunkReader::new(sealed, s.clone()).read_to_end(&mut out)?;
        Ok(out)
    }

    /// The sealed chunks of a stream: (offset of the length, total bytes with the length).
    fn chunks_of(sealed: &[u8]) -> Vec<(usize, usize)> {
        let mut at = 0;
        let mut found = Vec::new();
        while at < sealed.len() {
            let len = u32::from_le_bytes(sealed[at..at + 4].try_into().unwrap()) as usize;
            found.push((at, 4 + len));
            at += 4 + len;
        }
        found
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn password_length_is_counted_in_characters() {
        assert!(matches!(
            check_password(&"a".repeat(9)),
            Err(BackupError::PasswordShort(10))
        ));
        assert!(check_password(&"a".repeat(10)).is_ok());
        assert!(check_password(&"a".repeat(256)).is_ok());
        assert!(matches!(
            check_password(&"a".repeat(257)),
            Err(BackupError::PasswordLong(256))
        ));
        // Ten Cyrillic letters are twenty bytes and still ten characters.
        assert!(matches!(
            check_password(&"я".repeat(9)),
            Err(BackupError::PasswordShort(10))
        ));
        assert!(check_password(&"я".repeat(10)).is_ok());
        assert!(check_password(&"я".repeat(256)).is_ok());
        assert!(check_password(&"я".repeat(257)).is_err());
        assert!(check_password("").is_err());
        // Nothing is trimmed: spaces are characters like any other.
        assert!(check_password("          ").is_ok());
    }

    #[test]
    fn chunk_stream_round_trips_at_every_border() {
        let s = sealing(4096);
        for len in [0, 1, 4095, 4096, 4097, 3 * 4096, 3 * 4096 + 5] {
            let data = pattern(len);
            let sealed = seal_all(&s, &data);
            assert_eq!(open_all(&s, &sealed).unwrap(), data, "{len} bytes");
            // A full buffer is sealed only when more data follows.
            let expected = if len == 0 { 1 } else { len.div_ceil(4096) };
            assert_eq!(chunks_of(&sealed).len(), expected, "{len} bytes");
        }
    }

    #[test]
    fn chunk_stream_round_trips_with_the_real_chunk_size() {
        let s = sealing(CHUNK);
        for len in [0, 1, CHUNK, CHUNK + 1, 2 * CHUNK + 17] {
            let data = pattern(len);
            let sealed = seal_all(&s, &data);
            assert_eq!(open_all(&s, &sealed).unwrap(), data, "{len} bytes");
            let expected = if len == 0 { 1 } else { len.div_ceil(CHUNK) };
            assert_eq!(chunks_of(&sealed).len(), expected, "{len} bytes");
        }
    }

    #[test]
    fn a_changed_or_reordered_stream_does_not_open() {
        let s = sealing(4096);
        let data = pattern(3 * 4096);
        let sealed = seal_all(&s, &data);
        let chunks = chunks_of(&sealed);
        assert_eq!(chunks.len(), 3);
        let invalid = |bytes: &[u8]| {
            let e = open_all(&s, bytes).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{e}");
        };

        // A flipped ciphertext byte and a flipped tag byte.
        let mut flipped = sealed.clone();
        flipped[chunks[1].0 + 4 + 10] ^= 1;
        invalid(&flipped);
        let mut tag = sealed.clone();
        let end_of_first = chunks[0].0 + chunks[0].1;
        tag[end_of_first - 1] ^= 0x80;
        invalid(&tag);

        // Two chunks swapped.
        let part = |i: usize| &sealed[chunks[i].0..chunks[i].0 + chunks[i].1];
        let swapped = [part(1), part(0), part(2)].concat();
        invalid(&swapped);

        // Cut at a chunk border: the last chunk is missing.
        invalid(&sealed[..chunks[2].0]);
        invalid(&sealed[..chunks[1].0]);
        // Cut inside a chunk and inside a length.
        invalid(&sealed[..sealed.len() - 1]);
        invalid(&sealed[..chunks[1].0 + 2]);
        // Nothing at all.
        invalid(&[]);
        // A chunk repeated after the last one.
        let repeated = [&sealed[..], part(2)].concat();
        invalid(&repeated);
        // An impossible length.
        let mut huge = sealed.clone();
        huge[..4].copy_from_slice(&(4096u32 + 17).to_le_bytes());
        invalid(&huge);
        let mut tiny = sealed.clone();
        tiny[..4].copy_from_slice(&15u32.to_le_bytes());
        invalid(&tiny);

        // Another header hash or another nonce opens nothing either.
        let mut other = s.clone();
        other.aad[0] ^= 1;
        assert!(open_all(&other, &sealed).is_err());
        let mut other = s.clone();
        other.nonce[0] ^= 1;
        assert!(open_all(&other, &sealed).is_err());
        assert_eq!(open_all(&s, &sealed).unwrap(), data);
    }

    #[test]
    fn nothing_is_handed_out_before_its_chunk_is_checked() {
        let s = sealing(4096);
        let mut sealed = seal_all(&s, &pattern(4096 + 100));
        let chunks = chunks_of(&sealed);
        // Damage the second chunk: the first still reads, then the error comes.
        sealed[chunks[1].0 + 4] ^= 1;
        let mut r = ChunkReader::new(&sealed[..], s.clone());
        let mut first = vec![0u8; 4096];
        r.read_exact(&mut first).unwrap();
        assert_eq!(first, pattern(4096));
        let mut rest = Vec::new();
        assert!(r.read_to_end(&mut rest).is_err());
        assert!(rest.is_empty());
    }

    #[test]
    fn the_check_value_tells_a_wrong_password_apart() {
        let salt = [3u8; SALT_LEN];
        let good = test_keys("correct horse", &salt);
        let check = good.check_value();
        assert!(good.matches(&check));
        assert!(test_keys("correct horse", &salt).matches(&check));
        assert!(!test_keys("correct horsf", &salt).matches(&check));
        // Not trimmed and not normalised.
        assert!(!test_keys("correct horse ", &salt).matches(&check));
        assert!(!test_keys("Correct horse", &salt).matches(&check));
        // Another salt gives other keys.
        assert!(!test_keys("correct horse", &[4u8; SALT_LEN]).matches(&check));
        assert!(!good.matches(&check[..31]));
    }

    #[test]
    fn random_bytes_differ_between_calls() {
        let a: [u8; SALT_LEN] = random().unwrap();
        let b: [u8; SALT_LEN] = random().unwrap();
        assert_ne!(a, b);
    }
}
