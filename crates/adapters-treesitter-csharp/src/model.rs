use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CsharpAnalysis {
    pub(super) definitions: Vec<Definition>,
    pub(super) parse_error_lines: Vec<usize>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) enum DefinitionKind {
    Namespace,
    Type,
    Callable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Definition {
    pub(super) qualified_name: String,
    pub(super) kind: DefinitionKind,
    pub(super) return_type: Option<String>,
    pub(super) base_types: Vec<String>,
    pub(super) line: usize,
    pub(super) parameters: Vec<Parameter>,
    pub(super) locals: Vec<Binding>,
    pub(super) calls: Vec<Call>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Binding {
    pub(super) name: String,
    pub(super) type_name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Parameter {
    pub(super) name: String,
    pub(super) type_name: String,
    pub(super) is_extension: bool,
    pub(super) is_optional: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Argument {
    pub(super) name: Option<String>,
    pub(super) expression: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) enum CallKind {
    Direct,
    Member,
    Constructor,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Call {
    pub(super) expression: String,
    pub(super) kind: CallKind,
    pub(super) receiver: Option<String>,
    pub(super) name: String,
    pub(super) type_arguments: Vec<String>,
    pub(super) arguments: Vec<Argument>,
    pub(super) line: usize,
}
