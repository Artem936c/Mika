//! The description (JSON) of a binary glTF file.

use std::io::{Read, Seek, SeekFrom};

use super::ModelError;

/// The longest description read. A real model has a few hundred kilobytes.
pub const MAX_JSON_BYTES: u32 = 16 * 1024 * 1024;

/// Reads the description of the model: the first chunk of the file. The
/// meshes and pictures after it are stepped over, not read.
///
/// A file with a second description is refused: a loader takes the last one,
/// so the one checked here would not be the one shown.
pub fn read_json(reader: &mut (impl Read + Seek)) -> Result<serde_json::Value, ModelError> {
    let end = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;
    let mut magic = [0u8; 4];
    fill(reader, &mut magic).map_err(|e| match e {
        ModelError::Broken => ModelError::NotGlb,
        other => other,
    })?;
    if u32::from_le_bytes(magic) != MAGIC {
        return Err(ModelError::NotGlb);
    }
    // Version and total length, then the length and the type of the first chunk.
    let mut rest = [0u8; 16];
    fill(reader, &mut rest)?;
    let word = |at: usize| u32::from_le_bytes([rest[at], rest[at + 1], rest[at + 2], rest[at + 3]]);
    if word(0) != 2 {
        return Err(ModelError::NotGlb);
    }
    let (len, kind) = (word(8), word(12));
    // The length is the file's word: checked before any byte of the chunk is read.
    if kind != JSON_CHUNK || len > MAX_JSON_BYTES {
        return Err(ModelError::Broken);
    }
    let mut body = Vec::new();
    reader.take(u64::from(len)).read_to_end(&mut body)?;
    if body.len() != len as usize {
        return Err(ModelError::Broken);
    }
    let json = serde_json::from_slice(&body).map_err(|_| ModelError::Broken)?;

    // The rest of the file, chunk header by chunk header.
    let mut at = HEADER_BYTES + CHUNK_HEADER_BYTES + u64::from(len);
    for _ in 0..MAX_CHUNKS {
        if at == end {
            return Ok(json);
        }
        if end - at < CHUNK_HEADER_BYTES {
            return Err(ModelError::Broken);
        }
        reader.seek(SeekFrom::Start(at))?;
        let mut header = [0u8; 8];
        fill(reader, &mut header)?;
        let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        let kind = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        at += CHUNK_HEADER_BYTES + u64::from(len);
        if kind == JSON_CHUNK || at > end {
            return Err(ModelError::Broken);
        }
    }
    Err(ModelError::Broken)
}

const MAGIC: u32 = 0x4654_6C67;
const JSON_CHUNK: u32 = 0x4E4F_534A;
const HEADER_BYTES: u64 = 12;
const CHUNK_HEADER_BYTES: u64 = 8;
/// A model has the description and one chunk of data; a few more are let be.
const MAX_CHUNKS: usize = 16;

/// Fills the buffer; a file that ends too early is broken.
fn fill(reader: &mut impl Read, buffer: &mut [u8]) -> Result<(), ModelError> {
    reader.read_exact(buffer).map_err(|e| match e.kind() {
        std::io::ErrorKind::UnexpectedEof => ModelError::Broken,
        _ => ModelError::Io(e),
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::glb;
    use super::*;

    fn read(bytes: &[u8]) -> Result<serde_json::Value, ModelError> {
        read_json(&mut std::io::Cursor::new(bytes))
    }

    const BIN: u32 = 0x004E_4942;

    /// Adds a chunk to a file and makes the header name the new length.
    fn with_chunk(mut file: Vec<u8>, kind: u32, body: &[u8]) -> Vec<u8> {
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend_from_slice(&kind.to_le_bytes());
        file.extend_from_slice(body);
        let total = file.len() as u32;
        file[8..12].copy_from_slice(&total.to_le_bytes());
        file
    }

    #[test]
    fn a_second_description_is_refused() {
        // The loader of the page takes the last description in a file: a
        // clean first one must not hide another that points outside.
        let hidden = br#"{"images":[{"uri":"https://x.example/t.png"}]}  "#;
        let right_after = with_chunk(glb("{}"), JSON_CHUNK, hidden);
        assert!(matches!(read(&right_after), Err(ModelError::Broken)));
        let after_the_data = with_chunk(with_chunk(glb("{}"), BIN, &[0u8; 8]), JSON_CHUNK, hidden);
        assert!(matches!(read(&after_the_data), Err(ModelError::Broken)));
    }

    #[test]
    fn the_data_after_the_description_is_passed_over() {
        let file = with_chunk(glb(r#"{"a":1}"#), BIN, &[7u8; 12]);
        assert_eq!(read(&file).unwrap(), serde_json::json!({ "a": 1 }));
        // A chunk of a kind we do not know is not ours to judge.
        let file = with_chunk(file, 0x1234_5678, &[0u8; 4]);
        assert_eq!(read(&file).unwrap(), serde_json::json!({ "a": 1 }));
    }

    #[test]
    fn a_chunk_that_runs_past_the_end_is_broken() {
        let mut file = with_chunk(glb("{}"), BIN, &[0u8; 4]);
        let at = file.len() - 12;
        file[at..at + 4].copy_from_slice(&100u32.to_le_bytes());
        assert!(matches!(read(&file), Err(ModelError::Broken)));
        // Half a chunk header at the end.
        let mut file = glb("{}");
        file.extend_from_slice(&[0u8; 5]);
        assert!(matches!(read(&file), Err(ModelError::Broken)));
    }

    #[test]
    fn a_file_of_a_thousand_chunks_is_broken() {
        let mut file = glb("{}");
        for _ in 0..1000 {
            file = with_chunk(file, BIN, &[]);
        }
        assert!(matches!(read(&file), Err(ModelError::Broken)));
    }

    #[test]
    fn a_json_chunk_is_read_back() {
        let file = glb(r#"{"asset":{"version":"2.0"}}"#);
        assert_eq!(
            read(&file).unwrap(),
            serde_json::json!({ "asset": { "version": "2.0" } })
        );
    }

    #[test]
    fn a_file_that_is_not_gltf_is_refused() {
        assert!(matches!(
            read(b"PK\x03\x04 some zip archive, long enough"),
            Err(ModelError::NotGlb)
        ));
        assert!(matches!(read(b""), Err(ModelError::NotGlb)));
        // The first version of the format is another container.
        let mut old = glb("{}");
        old[4..8].copy_from_slice(&1u32.to_le_bytes());
        assert!(matches!(read(&old), Err(ModelError::NotGlb)));
    }

    #[test]
    fn a_cut_file_is_broken() {
        let file = glb(r#"{"asset":{"version":"2.0"}}"#);
        for len in [10, 15, file.len() - 1] {
            assert!(
                matches!(read(&file[..len]), Err(ModelError::Broken)),
                "cut at {len}"
            );
        }
    }

    #[test]
    fn a_lying_length_is_refused_before_reading() {
        for len in [u32::MAX, MAX_JSON_BYTES + 1] {
            let mut file = glb("{}");
            file[12..16].copy_from_slice(&len.to_le_bytes());
            assert!(matches!(read(&file), Err(ModelError::Broken)), "{len}");
        }
    }

    #[test]
    fn a_first_chunk_that_is_not_json_is_broken() {
        let mut file = glb("{}");
        file[16..20].copy_from_slice(&0x004E_4942u32.to_le_bytes());
        assert!(matches!(read(&file), Err(ModelError::Broken)));
        assert!(matches!(read(&glb("not json")), Err(ModelError::Broken)));
    }
}
