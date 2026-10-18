//! Wire format between the app and the helper process: one JSON object per line on the
//! helper's stdin/stdout. Audio never travels on the pipe; it goes through a shared file mapping
//! (interleaved stereo f32: the input block, then the output block).
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Request {
    /// Load the library, list every plugin in it with its parameters, unload again (scan).
    Info {
        path: String,
    },
    /// Load, create the plugin `id`, activate it; the helper keeps it until `Quit`.
    Load {
        path: String,
        id: String,
        rate: u32,
        max_frames: u32,
    },
    /// Queue a parameter change; it reaches the plugin as an event with the next block.
    Set {
        index: u32,
        value: f64,
    },
    Get {
        index: u32,
    },
    /// Process `frames` stereo frames from the mapping at `shm`. `sets` are parameter changes
    /// (index, value) applied before the block: fire-and-forget, no separate reply.
    Process {
        shm: String,
        frames: u32,
        #[serde(default)]
        sets: Vec<(u32, f64)>,
    },
    Quit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamInfo {
    pub id: u32,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub params: Vec<ParamInfo>,
    /// Has a stereo input and a stereo output port.
    #[serde(default = "yes")]
    pub stereo: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "re", rename_all = "lowercase")]
pub enum Response {
    Info { plugins: Vec<PluginInfo> },
    Loaded { params: Vec<ParamInfo> },
    Value { value: f64 },
    Done,
    Err { msg: String },
}
