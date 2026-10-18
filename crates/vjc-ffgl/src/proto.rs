//! Wire format between the app and the helper process: one JSON object per line on the
//! helper's stdin/stdout. Pixels never travel on the pipe; they go through a shared file mapping.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Request {
    /// Load the library, read its info and parameters, unload again (used by the scan).
    Info {
        path: String,
    },
    /// Load and instantiate; the helper keeps the instance until `Quit`.
    Load {
        path: String,
    },
    Set {
        index: u32,
        value: f32,
    },
    Get {
        index: u32,
    },
    /// Process one frame: input RGBA8 at offset 0, output RGBA8 right after it, in the mapping at `shm`.
    Process {
        shm: String,
        w: u32,
        h: u32,
    },
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Effect,
    Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamKind {
    Bool,
    Float,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamInfo {
    pub name: String,
    pub kind: ParamKind,
    pub default: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginInfo {
    /// Four character unique id.
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub params: Vec<ParamInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "re", rename_all = "lowercase")]
pub enum Response {
    Info { info: PluginInfo },
    Value { value: f32 },
    Done,
    Err { msg: String },
}
