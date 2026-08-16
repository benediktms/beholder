use beholder_domain::{AnalysisDiagnostic, AnalysisDiagnosticSeverity};
use std::collections::BTreeMap;

pub(super) fn report_analysis_diagnostics(
    workspace: &str,
    diagnostics: &[(String, AnalysisDiagnostic)],
) {
    let mut limitations = BTreeMap::<&str, usize>::new();
    for (repository, diagnostic) in diagnostics {
        match diagnostic.severity {
            AnalysisDiagnosticSeverity::KnownLimitation => {
                *limitations.entry(diagnostic.code.as_str()).or_default() += 1;
                tracing::debug!(
                    workspace,
                    repository,
                    code = %diagnostic.code,
                    severity = diagnostic.severity.as_str(),
                    path = %diagnostic.path.display(),
                    line = ?diagnostic.line,
                    detail = ?diagnostic.detail,
                    "frontend analysis diagnostic"
                );
            }
            AnalysisDiagnosticSeverity::Warning => tracing::warn!(
                workspace,
                repository,
                code = %diagnostic.code,
                severity = diagnostic.severity.as_str(),
                path = %diagnostic.path.display(),
                line = ?diagnostic.line,
                detail = ?diagnostic.detail,
                "frontend analysis diagnostic"
            ),
        }
    }
    if !limitations.is_empty() {
        tracing::info!(
            workspace,
            known_limitations = limitations.values().sum::<usize>(),
            codes = ?limitations,
            "frontend analysis limitations"
        );
    }
}
