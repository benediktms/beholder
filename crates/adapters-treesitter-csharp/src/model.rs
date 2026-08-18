use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CsharpAnalysis {
    pub(super) definitions: Vec<Definition>,
    pub(super) parse_error_lines: Vec<usize>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) enum DefinitionKind {
    Namespace,
    Callable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Definition {
    pub(super) qualified_name: String,
    pub(super) kind: DefinitionKind,
    pub(super) line: usize,
    pub(super) calls: Vec<Call>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) enum CallKind {
    Direct,
    Member,
    Constructor,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Call {
    pub(super) kind: CallKind,
    pub(super) receiver: Option<String>,
    pub(super) name: String,
    pub(super) line: usize,
}
