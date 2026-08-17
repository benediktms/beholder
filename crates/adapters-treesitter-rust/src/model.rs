use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RustAnalysis {
    pub(super) functions: Vec<RustFunction>,
    pub(super) tonic: TonicAnalysis,
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
