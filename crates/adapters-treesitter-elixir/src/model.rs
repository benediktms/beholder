use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::PathBuf};

use super::grpc::GrpcModule;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ElixirAnalysis {
    pub(super) modules: Vec<ElixirModule>,
    pub(super) parse_error_lines: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct ElixirRepository {
    pub(super) repository: String,
    pub(super) sources: Vec<(PathBuf, ElixirAnalysis)>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct ElixirModule {
    pub(super) name: String,
    pub(super) enclosing_module: Option<String>,
    pub(super) line: usize,
    pub(super) functions: Vec<ElixirFunction>,
    pub(super) callbacks: Vec<ElixirFunction>,
    pub(super) using_functions: Vec<ElixirFunction>,
    pub(super) using_implements: Vec<ElixirModuleReference>,
    pub(super) struct_fields: Vec<ElixirStructField>,
    pub(super) implements: Vec<ElixirModuleReference>,
    pub(super) aliases: Vec<ElixirAlias>,
    pub(super) references: Vec<ElixirModuleReference>,
    pub(super) grpc: GrpcModule,
    pub(super) absinthe_resolvers: Vec<AbsintheResolver>,
    pub(super) absinthe_field_imports: Vec<AbsintheFieldImport>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct AbsintheResolver {
    pub(super) field: String,
    pub(super) public_field: Option<String>,
    pub(super) owner: String,
    pub(super) parent: Option<String>,
    pub(super) module: String,
    pub(super) function: String,
    pub(super) arity: usize,
    pub(super) line: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct AbsintheFieldImport {
    pub(super) imported: String,
    pub(super) parent: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct ElixirFunction {
    pub(super) name: String,
    pub(super) arity: usize,
    pub(super) line: usize,
    pub(super) calls: Vec<ElixirCall>,
    pub(super) struct_uses: Vec<ElixirStructUse>,
    pub(super) imports: Vec<ElixirModuleReference>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct ElixirCall {
    pub(super) module: Option<String>,
    pub(super) name: String,
    pub(super) arity: usize,
    pub(super) line: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct ElixirAlias {
    pub(super) name: String,
    pub(super) target: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct ElixirStructField {
    pub(super) name: String,
    pub(super) line: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct ElixirStructUse {
    pub(super) module: String,
    pub(super) line: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) enum ElixirModuleReferenceKind {
    Behaviour,
    Import,
    Require,
    Use,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct ElixirModuleReference {
    pub(super) name: String,
    pub(super) kind: ElixirModuleReferenceKind,
    pub(super) line: usize,
    pub(super) only: Option<BTreeSet<String>>,
    pub(super) except: BTreeSet<String>,
}
