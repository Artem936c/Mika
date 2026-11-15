//! Avatar models the user brings: the check of the file, the slots of the
//! ten emotions, the profile and the folder the models live in.
//!
//! A model file is somebody else's data. Nothing in it may make the program
//! reach the network or another file: `vrm::inspect` refuses such a model,
//! at import and again at every load.

pub mod glb;
pub mod profile;
pub mod slots;
pub mod store;
pub mod vrm;

pub use profile::Profile;
pub use slots::{Slot, Source};
pub use store::{is_model_id, still_names, ModelEntry, Store};
pub use vrm::{inspect, ModelInfo, Spec};

/// The biggest model file taken.
pub const MAX_MODEL_BYTES: u64 = 200 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("not a binary glTF 2 file")]
    NotGlb,
    #[error("no VRM extension")]
    NotVrm,
    #[error("the file is too big")]
    TooBig,
    #[error("the model refers to outside files")]
    External,
    #[error("unsupported extension: {0}")]
    Unsupported(String),
    #[error("the model file is broken")]
    Broken,
    #[error("unknown model")]
    Unknown,
    #[error("bad picture")]
    BadStill,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl ModelError {
    /// A short name of the error for the journal and for the key of its text.
    pub fn kind(&self) -> &'static str {
        match self {
            ModelError::NotGlb => "not_glb",
            ModelError::NotVrm => "not_vrm",
            ModelError::TooBig => "too_big",
            ModelError::External => "external",
            ModelError::Unsupported(_) => "unsupported",
            ModelError::Broken => "broken",
            ModelError::Unknown => "unknown",
            ModelError::BadStill => "bad_still",
            ModelError::Io(_) => "io",
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    /// A binary glTF 2 file with this description and nothing else.
    pub fn glb(json: &str) -> Vec<u8> {
        let mut body = json.as_bytes().to_vec();
        while !body.len().is_multiple_of(4) {
            body.push(b' ');
        }
        let total = (12 + 8 + body.len()) as u32;
        let mut out = Vec::with_capacity(total as usize);
        out.extend_from_slice(&0x4654_6C67u32.to_le_bytes());
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&total.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// The description of a small VRM 1.0 model with these expressions.
    pub fn vrm1(presets: &[&str], custom: &[&str]) -> serde_json::Value {
        let map = |names: &[&str]| {
            names
                .iter()
                .map(|n| (n.to_string(), serde_json::json!({})))
                .collect::<serde_json::Map<_, _>>()
        };
        serde_json::json!({
            "asset": { "version": "2.0" },
            "extensionsUsed": ["VRMC_vrm"],
            "extensions": { "VRMC_vrm": {
                "specVersion": "1.0",
                "meta": { "name": "Test", "authors": ["Somebody"], "licenseUrl": "https://vrm.dev/licenses/1.0/" },
                "expressions": { "preset": map(presets), "custom": map(custom) }
            } }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_has_its_own_kind() {
        let all = [
            ModelError::NotGlb,
            ModelError::NotVrm,
            ModelError::TooBig,
            ModelError::External,
            ModelError::Unsupported("x".into()),
            ModelError::Broken,
            ModelError::Unknown,
            ModelError::BadStill,
            ModelError::Io(std::io::Error::other("x")),
        ];
        let kinds: std::collections::HashSet<_> = all.iter().map(ModelError::kind).collect();
        assert_eq!(kinds.len(), all.len());
    }
}
