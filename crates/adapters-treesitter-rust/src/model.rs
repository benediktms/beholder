use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustAnalysis {
    pub(super) functions: Vec<RustFunction>,
    #[serde(default)]
    pub(super) module_hash: [u8; 32],
    pub(super) tonic: TonicAnalysis,
    pub(super) parse_error_lines: Vec<usize>,
}

impl RustAnalysis {
    pub fn functions(&self) -> impl Iterator<Item = &RustFunction> {
        self.functions.iter()
    }

    pub fn module_hash(&self) -> [u8; 32] {
        self.module_hash
    }
}

pub(super) struct RustRepository {
    pub(super) repository: String,
    pub(super) sources: Vec<(PathBuf, RustAnalysis)>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct TonicAnalysis {
    pub(super) packages: Vec<String>,
    pub(super) client_calls: Vec<TonicBinding>,
    pub(super) server_methods: Vec<TonicBinding>,
    pub(super) generated_methods: Vec<TonicGeneratedMethod>,
    pub(super) recognized_receiver_calls: Vec<(usize, String)>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct TonicBinding {
    pub(super) function: String,
    pub(super) service: String,
    pub(super) method: String,
    pub(super) line: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct TonicGeneratedMethod {
    pub(super) service: String,
    pub(super) method: String,
    pub(super) line: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustFunction {
    pub(super) name: String,
    pub(super) qualified_name: String,
    #[serde(default)]
    pub(super) interface_hash: [u8; 32],
    #[serde(default)]
    pub(super) body_hash: [u8; 32],
    pub(super) line: usize,
    #[serde(default)]
    pub(super) name_offset: usize,
    pub(super) calls: Vec<RustCall>,
}

impl RustFunction {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn qualified_name(&self) -> &str {
        &self.qualified_name
    }

    pub fn interface_hash(&self) -> [u8; 32] {
        self.interface_hash
    }

    pub fn body_hash(&self) -> [u8; 32] {
        self.body_hash
    }

    pub fn name_offset(&self) -> usize {
        self.name_offset
    }

    pub fn calls(&self) -> impl Iterator<Item = &RustCall> {
        self.calls.iter()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustCall {
    pub(super) name: String,
    pub(super) line: usize,
    #[serde(default)]
    pub(super) offset: usize,
    pub(super) receiver_method: bool,
}

impl RustCall {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn receiver_method(&self) -> bool {
        self.receiver_method
    }
}
