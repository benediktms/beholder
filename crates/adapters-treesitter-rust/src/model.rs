use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustAnalysis {
    pub(super) functions: Vec<RustFunction>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct RustFunction {
    pub(super) name: String,
    pub(super) qualified_name: String,
    pub(super) line: usize,
    pub(super) calls: Vec<RustCall>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct RustCall {
    pub(super) name: String,
    pub(super) line: usize,
    pub(super) receiver_method: bool,
}
