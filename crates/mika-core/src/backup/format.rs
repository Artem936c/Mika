//! The file around the payload: magic bytes, format number and a plain header.
//!
//! ```text
//! offset 0     8 bytes  MAGIC
//! offset 8     2 bytes  format, u16 little-endian
//! offset 10    4 bytes  header length H, u32 little-endian
//! offset 14    H bytes  header, one UTF-8 JSON object
//! offset 14+H           body, to the end of the file
//! ```
//!
//! The header is readable without a password: it says what the file is
//! before anything is asked. Its hash is signed into every encrypted chunk.

use std::io::{self, Read};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::crypto::{CHECK_LEN, NONCE_PREFIX};
use super::{
    BackupError, FORMAT, KDF_ITERATIONS_MAX, KDF_ITERATIONS_MIN, MAGIC, MAX_CHUNK, MIN_CHUNK,
};

const KDF_NAME: &str = "pbkdf2-hmac-sha256";
const CIPHER_NAME: &str = "aes-256-gcm";
/// Bytes before the header: magic, format, header length.
const FIXED_LEN: usize = 14;
/// Salt lengths a file may name, bytes.
const SALT_RANGE: std::ops::RangeInclusive<usize> = 16..=64;
/// How much of each header text is kept, characters.
const MAX_VERSION: usize = 32;
const MAX_DATE: usize = 40;
const MAX_DEVICE: usize = 64;

/// How much a backup file may hold. Over a limit the file is refused before
/// anything is changed.
#[derive(Debug, Clone)]
pub struct Limits {
    /// The whole file, bytes.
    pub file: u64,
    /// The plain header, bytes.
    pub header: usize,
    /// The decompressed payload, bytes.
    pub plain: u64,
    /// One line of the payload, bytes.
    pub line: usize,
    /// Rows of all tables together.
    pub rows: u64,
    /// The settings text, bytes.
    pub config: usize,
    /// Secret lines.
    pub secrets: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            file: 1 << 30,
            header: 64 << 10,
            plain: 2 << 30,
            line: 16 << 20,
            rows: 5_000_000,
            config: 1 << 20,
            secrets: 64,
        }
    }
}

/// How the body of an encrypted file was sealed. Nothing here is secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CryptoParams {
    pub iterations: u32,
    pub salt: Vec<u8>,
    pub nonce: [u8; NONCE_PREFIX],
    pub chunk: usize,
    pub check: [u8; CHECK_LEN],
}

/// What a backup file says about itself before it is opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub app_version: String,
    pub created_at: String,
    pub device: String,
    pub encrypted: bool,
    /// Keys inside; always 0 for a file that is not encrypted.
    pub secrets: u32,
    /// SHA-256 of the file up to the body.
    pub prefix_sha256: [u8; 32],
    pub(crate) crypto: Option<CryptoParams>,
}

fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// The first bytes of a new file, up to the body, and the header they hold.
pub(crate) fn encode_prefix(
    app_version: &str,
    created_at: &str,
    device: &str,
    secrets: u32,
    crypto: Option<CryptoParams>,
) -> (Vec<u8>, Header) {
    let mut header = Header {
        app_version: cut(app_version, MAX_VERSION),
        created_at: cut(created_at, MAX_DATE),
        device: cut(device, MAX_DEVICE),
        encrypted: crypto.is_some(),
        secrets: if crypto.is_some() { secrets } else { 0 },
        prefix_sha256: [0; 32],
        crypto,
    };
    let mut fields = json!({
        "app_version": header.app_version,
        "created_at": header.created_at,
        "device": header.device,
        "encrypted": header.encrypted,
        "secrets": header.secrets,
    });
    if let (Some(c), Some(map)) = (&header.crypto, fields.as_object_mut()) {
        map.insert("kdf".into(), KDF_NAME.into());
        map.insert("iterations".into(), c.iterations.into());
        map.insert("salt".into(), BASE64.encode(&c.salt).into());
        map.insert("cipher".into(), CIPHER_NAME.into());
        map.insert("nonce".into(), BASE64.encode(c.nonce).into());
        map.insert("chunk".into(), c.chunk.into());
        map.insert("check".into(), BASE64.encode(c.check).into());
    }
    let text = fields.to_string();
    let mut prefix = Vec::with_capacity(FIXED_LEN + text.len());
    prefix.extend_from_slice(&MAGIC);
    prefix.extend_from_slice(&FORMAT.to_le_bytes());
    prefix.extend_from_slice(&(text.len() as u32).to_le_bytes());
    prefix.extend_from_slice(text.as_bytes());
    header.prefix_sha256 = Sha256::digest(&prefix).into();
    (prefix, header)
}

/// Reads until the buffer is full or the input ends. Returns how much came.
fn read_some(input: &mut dyn Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match input.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(got)
}

fn text(map: &Map<String, Value>, key: &str, max: usize) -> Result<String, BackupError> {
    map.get(key)
        .and_then(Value::as_str)
        .map(|s| cut(s, max))
        .ok_or(BackupError::Damaged)
}

fn bytes(map: &Map<String, Value>, key: &str) -> Result<Vec<u8>, BackupError> {
    map.get(key)
        .and_then(Value::as_str)
        .and_then(|s| BASE64.decode(s).ok())
        .ok_or(BackupError::Unsupported)
}

fn crypto_params(map: &Map<String, Value>) -> Result<CryptoParams, BackupError> {
    let named = |key: &str, name: &str| map.get(key).and_then(Value::as_str) == Some(name);
    if !named("kdf", KDF_NAME) || !named("cipher", CIPHER_NAME) {
        return Err(BackupError::Unsupported);
    }
    let number = |key: &str| {
        map.get(key)
            .and_then(Value::as_u64)
            .ok_or(BackupError::Unsupported)
    };
    let iterations = number("iterations")?;
    let chunk = number("chunk")?;
    let salt = bytes(map, "salt")?;
    let nonce: [u8; NONCE_PREFIX] = bytes(map, "nonce")?
        .try_into()
        .map_err(|_| BackupError::Unsupported)?;
    let check: [u8; CHECK_LEN] = bytes(map, "check")?
        .try_into()
        .map_err(|_| BackupError::Unsupported)?;
    let iterations_ok =
        (u64::from(KDF_ITERATIONS_MIN)..=u64::from(KDF_ITERATIONS_MAX)).contains(&iterations);
    let chunk_ok = (MIN_CHUNK as u64..=MAX_CHUNK as u64).contains(&chunk);
    if !iterations_ok || !chunk_ok || !SALT_RANGE.contains(&salt.len()) {
        return Err(BackupError::Unsupported);
    }
    Ok(CryptoParams {
        iterations: iterations as u32,
        salt,
        nonce,
        chunk: chunk as usize,
        check,
    })
}

/// Reads the file up to the body. `max_header` bounds the JSON header.
pub(crate) fn read_header_with(
    input: &mut dyn Read,
    max_header: usize,
) -> Result<Header, BackupError> {
    let mut fixed = [0u8; FIXED_LEN];
    let got = read_some(input, &mut fixed)?;
    if got < MAGIC.len() || fixed[..MAGIC.len()] != MAGIC {
        return Err(BackupError::NotBackup);
    }
    if got < FIXED_LEN {
        return Err(BackupError::Damaged);
    }
    let format = u16::from_le_bytes([fixed[8], fixed[9]]);
    if format == 0 {
        return Err(BackupError::NotBackup);
    }
    if format > FORMAT {
        return Err(BackupError::Newer(format));
    }
    let len = u32::from_le_bytes([fixed[10], fixed[11], fixed[12], fixed[13]]) as usize;
    if !(2..=max_header).contains(&len) {
        return Err(BackupError::Damaged);
    }
    let mut raw = vec![0u8; len];
    if read_some(input, &mut raw)? < len {
        return Err(BackupError::Damaged);
    }
    let value: Value = serde_json::from_slice(&raw).map_err(|_| BackupError::Damaged)?;
    let map = value.as_object().ok_or(BackupError::Damaged)?;
    let app_version = text(map, "app_version", MAX_VERSION)?;
    let created_at = text(map, "created_at", MAX_DATE)?;
    let device = text(map, "device", MAX_DEVICE)?;
    let encrypted = map
        .get("encrypted")
        .and_then(Value::as_bool)
        .ok_or(BackupError::Damaged)?;
    let secrets = map
        .get("secrets")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or(BackupError::Damaged)?;
    let crypto = if encrypted {
        Some(crypto_params(map)?)
    } else {
        None
    };
    let mut hash = Sha256::new();
    hash.update(fixed);
    hash.update(&raw);
    Ok(Header {
        app_version,
        created_at,
        device,
        encrypted,
        // A file that is not encrypted carries no keys, whatever it claims.
        secrets: if encrypted { secrets } else { 0 },
        prefix_sha256: hash.finalize().into(),
        crypto,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::backup::crypto::tests::test_keys;
    use crate::backup::crypto::{ChunkReader, ChunkWriter, Sealing};
    use crate::backup::read_header;
    use std::io::Write;

    pub(crate) fn test_params(password: &str) -> CryptoParams {
        let salt = vec![5u8; 16];
        let check = test_keys(password, &salt).check_value();
        CryptoParams {
            iterations: KDF_ITERATIONS_MIN,
            salt,
            nonce: [1, 2, 3, 4, 5, 6, 7],
            chunk: 1 << 20,
            check,
        }
    }

    /// A prefix whose JSON header is `header`.
    fn prefix_of(format: u16, header: &str) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(&format.to_le_bytes());
        out.extend_from_slice(&(header.len() as u32).to_le_bytes());
        out.extend_from_slice(header.as_bytes());
        out
    }

    fn read(bytes: &[u8]) -> Result<Header, BackupError> {
        read_header(&mut &bytes[..])
    }

    /// The JSON header of an encrypted file with one field replaced.
    fn encrypted_with(key: &str, value: Value) -> Vec<u8> {
        let (prefix, _) = encode_prefix(
            "0.3.3",
            "2026-11-14T15:30:00+05:00",
            "PC",
            2,
            Some(test_params("correct horse")),
        );
        let mut fields: Value = serde_json::from_slice(&prefix[FIXED_LEN..]).unwrap();
        fields[key] = value;
        prefix_of(FORMAT, &fields.to_string())
    }

    #[test]
    fn plain_header_round_trips_and_leaves_the_body() {
        let (mut file, written) =
            encode_prefix("0.3.3", "2026-11-14T15:30:00+05:00", "OLD-PC", 5, None);
        assert_eq!(&file[..8], b"MIKA-BKP");
        assert_eq!(&file[8..10], &[1, 0]);
        let prefix_len = file.len();
        file.extend_from_slice(b"BODY");
        let mut input = &file[..];
        let header = read_header(&mut input).unwrap();
        // Exactly the prefix is consumed.
        assert_eq!(input, b"BODY");
        assert_eq!(header, written);
        assert_eq!(header.app_version, "0.3.3");
        assert_eq!(header.created_at, "2026-11-14T15:30:00+05:00");
        assert_eq!(header.device, "OLD-PC");
        assert!(!header.encrypted && header.crypto.is_none());
        // A plain file never carries keys.
        assert_eq!(header.secrets, 0);
        let expected: [u8; 32] = Sha256::digest(&file[..prefix_len]).into();
        assert_eq!(header.prefix_sha256, expected);
    }

    #[test]
    fn encrypted_header_round_trips() {
        let params = test_params("correct horse");
        let (file, written) = encode_prefix(
            "0.3.3",
            "2026-11-14T15:30:00+05:00",
            "ПК-Дом",
            5,
            Some(params.clone()),
        );
        let header = read(&file).unwrap();
        assert_eq!(header, written);
        assert!(header.encrypted);
        assert_eq!(header.secrets, 5);
        assert_eq!(header.device, "ПК-Дом");
        assert_eq!(header.crypto, Some(params));
        let fields: Value = serde_json::from_slice(&file[FIXED_LEN..]).unwrap();
        assert_eq!(fields["kdf"], "pbkdf2-hmac-sha256");
        assert_eq!(fields["cipher"], "aes-256-gcm");
        assert_eq!(fields["chunk"], 1_048_576);
        assert_eq!(fields["nonce"], "AQIDBAUGBw==");
    }

    #[test]
    fn header_texts_are_cut_for_display() {
        let long = "я".repeat(200);
        let header = format!(
            r#"{{"app_version":"{long}","created_at":"{long}","device":"{long}","encrypted":false,"secrets":0,"later":[1,2]}}"#
        );
        let h = read(&prefix_of(1, &header)).unwrap();
        assert_eq!(h.app_version.chars().count(), 32);
        assert_eq!(h.created_at.chars().count(), 40);
        assert_eq!(h.device.chars().count(), 64);
        // And on the way out.
        let (_, written) = encode_prefix(&long, &long, &long, 0, None);
        assert_eq!(written.device.chars().count(), 64);
    }

    #[test]
    fn foreign_and_newer_files_are_told_apart() {
        let good =
            r#"{"app_version":"1","created_at":"d","device":"p","encrypted":false,"secrets":0}"#;
        assert!(read(&prefix_of(1, good)).is_ok());
        for junk in [
            &b""[..],
            b"MIKA",
            b"PK\x03\x04 some zip file",
            b"SQLite format 3\0",
        ] {
            assert!(
                matches!(read(junk), Err(BackupError::NotBackup)),
                "{junk:?}"
            );
        }
        let mut wrong = prefix_of(1, good);
        wrong[0] = b'm';
        assert!(matches!(read(&wrong), Err(BackupError::NotBackup)));
        assert!(matches!(
            read(&prefix_of(0, good)),
            Err(BackupError::NotBackup)
        ));
        assert!(matches!(
            read(&prefix_of(2, good)),
            Err(BackupError::Newer(2))
        ));
        assert!(matches!(
            read(&prefix_of(700, "whatever a later format holds")),
            Err(BackupError::Newer(700))
        ));
    }

    #[test]
    fn a_broken_header_is_damaged() {
        let good =
            r#"{"app_version":"1","created_at":"d","device":"p","encrypted":false,"secrets":0}"#;
        // The magic alone, and a cut header.
        assert!(matches!(read(b"MIKA-BKP\x01"), Err(BackupError::Damaged)));
        let whole = prefix_of(1, good);
        assert!(matches!(
            read(&whole[..whole.len() - 1]),
            Err(BackupError::Damaged)
        ));
        // Header length 0 and 1.
        assert!(matches!(read(&prefix_of(1, "")), Err(BackupError::Damaged)));
        assert!(matches!(
            read(&prefix_of(1, "{")),
            Err(BackupError::Damaged)
        ));
        // Over the limit: refused by the length alone.
        let mut huge = MAGIC.to_vec();
        huge.extend_from_slice(&1u16.to_le_bytes());
        huge.extend_from_slice(&((64u32 << 10) + 1).to_le_bytes());
        assert!(matches!(read(&huge), Err(BackupError::Damaged)));
        assert!(matches!(
            read_header_with(&mut &whole[..], good.len() - 1),
            Err(BackupError::Damaged)
        ));
        assert!(read_header_with(&mut &whole[..], good.len()).is_ok());
        for bad in [
            "[]",
            "12",
            "{}",
            "not json at all",
            r#"{"app_version":"1","created_at":"d","device":"p","encrypted":false}"#,
            r#"{"app_version":1,"created_at":"d","device":"p","encrypted":false,"secrets":0}"#,
            r#"{"app_version":"1","created_at":"d","device":"p","encrypted":"no","secrets":0}"#,
            r#"{"app_version":"1","created_at":"d","device":"p","encrypted":false,"secrets":-1}"#,
            r#"{"app_version":"1","created_at":"d","device":"p","encrypted":false,"secrets":"5"}"#,
            r#"{"app_version":"1","created_at":"d","device":"p","encrypted":false,"secrets":4294967296}"#,
        ] {
            assert!(
                matches!(read(&prefix_of(1, bad)), Err(BackupError::Damaged)),
                "{bad}"
            );
        }
    }

    #[test]
    fn unknown_encryption_is_refused() {
        assert!(read(&encrypted_with("secrets", json!(3))).is_ok());
        assert!(read(&encrypted_with("iterations", json!(100_000))).is_ok());
        assert!(read(&encrypted_with("iterations", json!(5_000_000))).is_ok());
        assert!(read(&encrypted_with("chunk", json!(4096))).is_ok());
        assert!(read(&encrypted_with("chunk", json!(4 << 20))).is_ok());
        assert!(read(&encrypted_with("salt", json!(BASE64.encode([1u8; 64])))).is_ok());
        for (key, value) in [
            ("kdf", json!("argon2id")),
            ("kdf", json!(null)),
            ("cipher", json!("x")),
            ("iterations", json!(99_999)),
            ("iterations", json!(5_000_001)),
            ("iterations", json!("600000")),
            ("nonce", json!(BASE64.encode([1u8; 6]))),
            ("nonce", json!(BASE64.encode([1u8; 8]))),
            ("nonce", json!("not base64 !")),
            ("chunk", json!(8 << 20)),
            ("chunk", json!(4095)),
            ("salt", json!(BASE64.encode([1u8; 15]))),
            ("salt", json!(BASE64.encode([1u8; 65]))),
            ("check", json!(BASE64.encode([1u8; 31]))),
            ("check", json!(7)),
        ] {
            assert!(
                matches!(
                    read(&encrypted_with(key, value.clone())),
                    Err(BackupError::Unsupported)
                ),
                "{key} = {value}"
            );
        }
        // An encrypted file without the parameters at all.
        let bare =
            r#"{"app_version":"1","created_at":"d","device":"p","encrypted":true,"secrets":0}"#;
        assert!(matches!(
            read(&prefix_of(1, bare)),
            Err(BackupError::Unsupported)
        ));
    }

    #[test]
    fn a_changed_header_fails_the_first_chunk_even_with_the_right_password() {
        let params = test_params("correct horse");
        let (prefix, header) = encode_prefix("0.3.3", "d", "PC", 5, Some(params.clone()));
        let keys = test_keys("correct horse", &params.salt);
        let sealing = |aad: [u8; 32]| Sealing {
            keys: keys.clone(),
            nonce: params.nonce,
            aad,
            chunk: params.chunk,
        };
        let mut w = ChunkWriter::new(Vec::new(), sealing(header.prefix_sha256));
        w.write_all(b"the payload").unwrap();
        let body = w.finish().unwrap();

        // Untouched: opens.
        let mut out = Vec::new();
        ChunkReader::new(&body[..], sealing(read(&prefix).unwrap().prefix_sha256))
            .read_to_end(&mut out)
            .unwrap();
        assert_eq!(out, b"the payload");

        // "secrets": 5 becomes 6. The header still reads and the password
        // still checks out, but no chunk opens.
        let text = String::from_utf8(prefix[FIXED_LEN..].to_vec()).unwrap();
        let edited = prefix_of(FORMAT, &text.replace(r#""secrets":5"#, r#""secrets":6"#));
        assert_eq!(edited.len(), prefix.len());
        let changed = read(&edited).unwrap();
        assert_eq!(changed.secrets, 6);
        let crypto = changed.crypto.clone().unwrap();
        assert!(keys.matches(&crypto.check));
        let e = ChunkReader::new(&body[..], sealing(changed.prefix_sha256))
            .read_to_end(&mut Vec::new())
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }
}
