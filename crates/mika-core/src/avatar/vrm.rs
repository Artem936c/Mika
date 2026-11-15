//! Is this description a VRM model we may show, and what is in it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ModelError;

/// The version of the VRM format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Spec {
    #[serde(rename = "0.x")]
    V0,
    #[serde(rename = "1.0")]
    V1,
}

/// What the file says about itself.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    pub spec: Spec,
    pub title: String,
    pub author: String,
    /// The terms of use as the file gives them, one line.
    pub license: String,
    /// Names of the expressions, VRM 1.0 names for the standard ones.
    pub expressions: Vec<String>,
}

/// Checks the description and reads the meta data and the expressions.
pub fn inspect(json: &Value) -> Result<ModelInfo, ModelError> {
    let extensions = json.get("extensions");
    let (spec, body) = match (
        extensions.and_then(|e| e.get("VRMC_vrm")),
        extensions.and_then(|e| e.get("VRM")),
    ) {
        (Some(body), _) => (Spec::V1, body),
        (None, Some(body)) => (Spec::V0, body),
        (None, None) => return Err(ModelError::NotVrm),
    };
    if refers_outside(json) {
        return Err(ModelError::External);
    }
    if let Some(name) = strings(json.get("extensionsRequired"))
        .into_iter()
        .find(|name| !SHOWN_EXTENSIONS.contains(&name.as_str()))
    {
        return Err(ModelError::Unsupported(cut(&name)));
    }
    let meta = body.get("meta");
    let text = |key: &str| {
        meta.and_then(|m| m.get(key))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let (title, author, license) = match spec {
        Spec::V1 => (
            text("name"),
            strings(meta.and_then(|m| m.get("authors"))).join(", "),
            ["licenseUrl", "avatarPermission", "commercialUsage"].map(text),
        ),
        Spec::V0 => (
            text("title"),
            text("author"),
            ["licenseName", "allowedUserName", "commercialUssageName"].map(text),
        ),
    };
    let license: Vec<String> = license.into_iter().filter(|s| !s.is_empty()).collect();
    Ok(ModelInfo {
        spec,
        title: cut(&title),
        author: cut(&author),
        license: cut(&license.join("; ")),
        expressions: match spec {
            Spec::V1 => expressions_v1(body),
            Spec::V0 => expressions_v0(body),
        },
    })
}

/// The standard expressions of VRM 1.0, in the order of the format.
pub const STANDARD: [&str; 18] = [
    "happy",
    "angry",
    "sad",
    "relaxed",
    "surprised",
    "aa",
    "ih",
    "ou",
    "ee",
    "oh",
    "blink",
    "blinkLeft",
    "blinkRight",
    "lookUp",
    "lookDown",
    "lookLeft",
    "lookRight",
    "neutral",
];

/// Extensions a model may require: the scene shows them.
const SHOWN_EXTENSIONS: [&str; 10] = [
    "VRMC_vrm",
    "VRM",
    "VRMC_springBone",
    "VRMC_springBone_extended_collider",
    "VRMC_node_constraint",
    "VRMC_materials_mtoon",
    "VRMC_materials_hdr_emissiveMultiplier",
    "KHR_materials_unlit",
    "KHR_texture_transform",
    "KHR_materials_emissive_strength",
];

/// Standard names of VRM 0.x and what they are called in 1.0.
const V0_NAMES: [(&str, &str); 17] = [
    ("a", "aa"),
    ("i", "ih"),
    ("u", "ou"),
    ("e", "ee"),
    ("o", "oh"),
    ("joy", "happy"),
    ("sorrow", "sad"),
    ("fun", "relaxed"),
    ("angry", "angry"),
    ("blink", "blink"),
    ("blink_l", "blinkLeft"),
    ("blink_r", "blinkRight"),
    ("lookup", "lookUp"),
    ("lookdown", "lookDown"),
    ("lookleft", "lookLeft"),
    ("lookright", "lookRight"),
    ("neutral", "neutral"),
];

/// Longest text taken from a file: a title, an author, the terms.
const MAX_TEXT: usize = 200;

fn cut(text: &str) -> String {
    text.chars().take(MAX_TEXT).collect()
}

fn strings(list: Option<&Value>) -> Vec<String> {
    list.and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// A buffer or a picture that lives outside the file: an address or a path.
fn refers_outside(json: &Value) -> bool {
    ["buffers", "images"].iter().any(|list| {
        json.get(list)
            .and_then(Value::as_array)
            .is_some_and(|items| {
                items.iter().any(|item| match item.get("uri") {
                    None | Some(Value::Null) => false,
                    Some(Value::String(uri)) => !uri
                        .get(..5)
                        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("data:")),
                    Some(_) => true,
                })
            })
    })
}

fn push_new(names: &mut Vec<String>, name: &str) {
    let name = cut(name.trim());
    if !name.is_empty() && !names.contains(&name) {
        names.push(name);
    }
}

fn expressions_v1(body: &Value) -> Vec<String> {
    let mut names = Vec::new();
    let keys = |part: &str| -> Vec<String> {
        body.get("expressions")
            .and_then(|e| e.get(part))
            .and_then(Value::as_object)
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default()
    };
    let preset = keys("preset");
    for name in STANDARD {
        if preset.iter().any(|p| p == name) {
            push_new(&mut names, name);
        }
    }
    for name in preset.iter().chain(keys("custom").iter()) {
        push_new(&mut names, name);
    }
    names
}

fn expressions_v0(body: &Value) -> Vec<String> {
    let mut names = Vec::new();
    let groups = body
        .get("blendShapeMaster")
        .and_then(|m| m.get("blendShapeGroups"))
        .and_then(Value::as_array);
    for group in groups.into_iter().flatten() {
        let text = |key: &str| group.get(key).and_then(Value::as_str).unwrap_or("");
        let preset = text("presetName").to_ascii_lowercase();
        match V0_NAMES.iter().find(|(old, _)| *old == preset) {
            Some((_, new)) => push_new(&mut names, new),
            None => push_new(&mut names, text("name")),
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::super::testing::vrm1;
    use super::*;
    use serde_json::json;

    fn vrm0(groups: Value) -> Value {
        json!({
            "asset": { "version": "2.0" },
            "extensionsUsed": ["VRM"],
            "extensions": { "VRM": {
                "meta": {
                    "title": "Old one", "author": "Me",
                    "licenseName": "CC_BY", "allowedUserName": "Everyone", "commercialUssageName": "Allow"
                },
                "blendShapeMaster": { "blendShapeGroups": groups }
            } }
        })
    }

    #[test]
    fn vrm_1_gives_its_meta_and_expressions() {
        let info = inspect(&vrm1(&["happy", "blink", "aa"], &["Blush"])).unwrap();
        assert_eq!(info.spec, Spec::V1);
        assert_eq!(info.title, "Test");
        assert_eq!(info.author, "Somebody");
        assert_eq!(info.license, "https://vrm.dev/licenses/1.0/");
        // Standard ones in the order of the format, then the model's own.
        assert_eq!(info.expressions, ["happy", "aa", "blink", "Blush"]);
    }

    #[test]
    fn vrm_0_names_become_vrm_1_names() {
        let info = inspect(&vrm0(json!([
            { "name": "Joy", "presetName": "joy" },
            { "name": "Sorrow", "presetName": "sorrow" },
            { "name": "A", "presetName": "a" },
            { "name": "Blink_L", "presetName": "blink_l" },
            { "name": "Shy", "presetName": "unknown" },
            { "name": "Joy again", "presetName": "joy" },
        ])))
        .unwrap();
        assert_eq!(info.spec, Spec::V0);
        assert_eq!(info.title, "Old one");
        assert_eq!(info.author, "Me");
        assert_eq!(info.license, "CC_BY; Everyone; Allow");
        assert_eq!(info.expressions, ["happy", "sad", "aa", "blinkLeft", "Shy"]);
    }

    #[test]
    fn a_plain_gltf_is_not_vrm() {
        assert!(matches!(
            inspect(&json!({ "asset": { "version": "2.0" } })),
            Err(ModelError::NotVrm)
        ));
        assert!(matches!(
            inspect(&json!({ "extensions": { "KHR_lights_punctual": {} } })),
            Err(ModelError::NotVrm)
        ));
    }

    #[test]
    fn an_outside_buffer_or_image_is_refused() {
        let mut model = vrm1(&["happy"], &[]);
        model["buffers"] =
            json!([{ "byteLength": 4 }, { "uri": "https://x.example/y.bin", "byteLength": 4 }]);
        assert!(matches!(inspect(&model), Err(ModelError::External)));

        let mut model = vrm1(&["happy"], &[]);
        model["images"] = json!([{ "uri": "tex.png" }]);
        assert!(matches!(inspect(&model), Err(ModelError::External)));

        // Data inside the file itself is fine.
        let mut model = vrm1(&["happy"], &[]);
        model["buffers"] =
            json!([{ "byteLength": 4 }, { "uri": "data:application/octet-stream;base64,AAAA" }]);
        model["images"] = json!([{ "bufferView": 0, "mimeType": "image/png" }, { "uri": "DATA:image/png;base64,AAAA" }]);
        assert!(inspect(&model).is_ok());
    }

    #[test]
    fn a_required_extension_we_cannot_show_is_refused() {
        let mut model = vrm1(&["happy"], &[]);
        model["extensionsRequired"] = json!(["VRMC_vrm", "KHR_draco_mesh_compression"]);
        match inspect(&model) {
            Err(ModelError::Unsupported(name)) => assert_eq!(name, "KHR_draco_mesh_compression"),
            other => panic!("{other:?}"),
        }
        model["extensionsRequired"] = json!(["VRMC_vrm", "VRMC_springBone", "KHR_materials_unlit"]);
        assert!(inspect(&model).is_ok());
    }

    #[test]
    fn a_model_without_expressions_is_accepted() {
        assert!(inspect(&vrm1(&[], &[])).unwrap().expressions.is_empty());
        let mut bare = vrm1(&[], &[]);
        bare["extensions"]["VRMC_vrm"] = json!({});
        let info = inspect(&bare).unwrap();
        assert!(info.expressions.is_empty());
        assert_eq!((info.title.as_str(), info.author.as_str()), ("", ""));
        assert!(inspect(&vrm0(json!([]))).unwrap().expressions.is_empty());
    }

    #[test]
    fn long_texts_of_the_file_are_cut() {
        let mut model = vrm1(&[], &[]);
        model["extensions"]["VRMC_vrm"]["meta"]["name"] = json!("я".repeat(500));
        assert_eq!(inspect(&model).unwrap().title.chars().count(), 200);
    }
}
