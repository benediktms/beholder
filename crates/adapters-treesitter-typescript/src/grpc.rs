use super::{model::TypescriptAnalysis, nestjs, ts_proto};
use beholder_domain::{AnalysisDiagnostic, GrpcBindingCandidate, Observation};
use std::path::Path;

pub(super) struct GeneratedGrpcMethod<'a> {
    pub(super) short_service: String,
    pub(super) service: String,
    pub(super) method: String,
    pub(super) source_method: String,
    pub(super) local_symbol: String,
    pub(super) path: &'a Path,
    pub(super) line: usize,
}

pub struct GrpcBindingInput<'a> {
    pub repository: &'a str,
    pub sources: &'a [(&'a Path, &'a TypescriptAnalysis)],
    pub observations: &'a [Observation],
}

pub(super) fn literal(value: &str) -> Option<&str> {
    let quote = value.as_bytes().first().copied()?;
    if !matches!(quote, b'\'' | b'"' | b'`') {
        return None;
    }
    value
        .strip_prefix(quote as char)?
        .strip_suffix(quote as char)
}

pub(super) fn module_id(repository: &str, path: &Path, analysis: &TypescriptAnalysis) -> String {
    let module = path
        .with_extension("")
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    format!(
        "repo://{repository}/{}/{module}",
        analysis.language.id_segment()
    )
}

pub fn bindings(
    input: GrpcBindingInput<'_>,
) -> (Vec<GrpcBindingCandidate>, Vec<AnalysisDiagnostic>) {
    let generated = ts_proto::grpc_methods(input.repository, input.sources);
    let mut candidates = ts_proto::client_bindings(&generated, input.observations);
    let (nest_bindings, diagnostics) = nestjs::bindings(
        input.repository,
        input.sources,
        &generated,
        input.observations,
    );
    candidates.extend(nest_bindings);
    (candidates, diagnostics)
}
