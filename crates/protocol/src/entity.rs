use crate::v1;
use beholder_dto as dto;

impl From<dto::Freshness> for v1::Freshness {
    fn from(value: dto::Freshness) -> Self {
        Self {
            stale: value.stale,
            indexing: value.indexing,
            dirty_repositories: value.dirty_repositories,
            enriching_repositories: value.enriching_repositories,
        }
    }
}

impl From<v1::Freshness> for dto::Freshness {
    fn from(value: v1::Freshness) -> Self {
        Self {
            stale: value.stale,
            indexing: value.indexing,
            dirty_repositories: value.dirty_repositories,
            enriching_repositories: value.enriching_repositories,
        }
    }
}

impl From<dto::AnalysisCompleteness> for v1::AnalysisCompleteness {
    fn from(value: dto::AnalysisCompleteness) -> Self {
        match value {
            dto::AnalysisCompleteness::Complete => Self::Complete,
            dto::AnalysisCompleteness::Incomplete => Self::Incomplete,
        }
    }
}

fn analysis_completeness(value: i32) -> Result<dto::AnalysisCompleteness, &'static str> {
    match v1::AnalysisCompleteness::try_from(value).map_err(|_| "unknown analysis completeness")? {
        v1::AnalysisCompleteness::Unspecified => Err("analysis completeness is missing"),
        v1::AnalysisCompleteness::Complete => Ok(dto::AnalysisCompleteness::Complete),
        v1::AnalysisCompleteness::Incomplete => Ok(dto::AnalysisCompleteness::Incomplete),
    }
}

impl From<dto::AnalysisDiagnosticSeverity> for v1::AnalysisDiagnosticSeverity {
    fn from(value: dto::AnalysisDiagnosticSeverity) -> Self {
        match value {
            dto::AnalysisDiagnosticSeverity::KnownLimitation => Self::KnownLimitation,
            dto::AnalysisDiagnosticSeverity::Warning => Self::Warning,
        }
    }
}

fn analysis_diagnostic_severity(
    value: i32,
) -> Result<dto::AnalysisDiagnosticSeverity, &'static str> {
    match v1::AnalysisDiagnosticSeverity::try_from(value)
        .map_err(|_| "unknown analysis diagnostic severity")?
    {
        v1::AnalysisDiagnosticSeverity::Unspecified => {
            Err("analysis diagnostic severity is missing")
        }
        v1::AnalysisDiagnosticSeverity::KnownLimitation => {
            Ok(dto::AnalysisDiagnosticSeverity::KnownLimitation)
        }
        v1::AnalysisDiagnosticSeverity::Warning => Ok(dto::AnalysisDiagnosticSeverity::Warning),
    }
}

impl From<dto::AnalysisDiagnostic> for v1::AnalysisDiagnostic {
    fn from(value: dto::AnalysisDiagnostic) -> Self {
        Self {
            code: value.code,
            severity: v1::AnalysisDiagnosticSeverity::from(value.severity) as i32,
            repository: value.repository,
            path: value.path.to_string_lossy().into_owned(),
            line: value.line,
            detail: value.detail,
        }
    }
}

impl TryFrom<v1::AnalysisDiagnostic> for dto::AnalysisDiagnostic {
    type Error = &'static str;

    fn try_from(value: v1::AnalysisDiagnostic) -> Result<Self, Self::Error> {
        Ok(Self {
            code: value.code,
            severity: analysis_diagnostic_severity(value.severity)?,
            repository: value.repository,
            path: value.path.into(),
            line: value.line,
            detail: value.detail,
        })
    }
}

impl From<dto::QueryMetadata> for v1::QueryMetadata {
    fn from(value: dto::QueryMetadata) -> Self {
        Self {
            revision: value.revision,
            view: value.view,
            freshness: Some(value.freshness.into()),
            completeness: v1::AnalysisCompleteness::from(value.analysis.completeness) as i32,
            diagnostics: value
                .analysis
                .diagnostics
                .into_iter()
                .map(Into::into)
                .collect(),
            diagnostic_counts: Some(v1::DiagnosticCounts {
                total: value.analysis.diagnostic_counts.total,
                known_limitations: value.analysis.diagnostic_counts.known_limitations,
                warnings: value.analysis.diagnostic_counts.warnings,
            }),
        }
    }
}

impl TryFrom<v1::QueryMetadata> for dto::QueryMetadata {
    type Error = &'static str;

    fn try_from(value: v1::QueryMetadata) -> Result<Self, Self::Error> {
        let diagnostics = value
            .diagnostics
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<dto::AnalysisDiagnostic>, _>>()?;
        Ok(Self {
            revision: value.revision,
            view: value.view,
            freshness: value.freshness.ok_or("query freshness is missing")?.into(),
            analysis: dto::AnalysisMetadata {
                completeness: analysis_completeness(value.completeness)?,
                diagnostic_counts: value
                    .diagnostic_counts
                    .map(|counts| dto::DiagnosticCounts {
                        total: counts.total,
                        known_limitations: counts.known_limitations,
                        warnings: counts.warnings,
                    })
                    .unwrap_or_else(|| dto::DiagnosticCounts {
                        total: diagnostics.len() as u64,
                        known_limitations: diagnostics
                            .iter()
                            .filter(|row| {
                                row.severity == dto::AnalysisDiagnosticSeverity::KnownLimitation
                            })
                            .count() as u64,
                        warnings: diagnostics
                            .iter()
                            .filter(|row| row.severity == dto::AnalysisDiagnosticSeverity::Warning)
                            .count() as u64,
                    }),
                diagnostics,
            },
        })
    }
}

impl From<dto::TraversalMetadata> for v1::TraversalMetadata {
    fn from(value: dto::TraversalMetadata) -> Self {
        Self {
            max_hops: value.max_hops,
            truncated: value.truncated,
        }
    }
}

impl From<v1::TraversalMetadata> for dto::TraversalMetadata {
    fn from(value: v1::TraversalMetadata) -> Self {
        Self {
            max_hops: value.max_hops,
            truncated: value.truncated,
        }
    }
}

impl From<dto::EntityKind> for v1::EntityKind {
    fn from(value: dto::EntityKind) -> Self {
        match value {
            dto::EntityKind::Callable => Self::Callable,
            dto::EntityKind::GraphqlArgument => Self::GraphqlArgument,
            dto::EntityKind::GraphqlEnumValue => Self::GraphqlEnumValue,
            dto::EntityKind::GraphqlField => Self::GraphqlField,
            dto::EntityKind::GraphqlOperation => Self::GraphqlOperation,
            dto::EntityKind::GraphqlType => Self::GraphqlType,
            dto::EntityKind::KafkaTopic => Self::KafkaTopic,
            dto::EntityKind::Namespace => Self::Namespace,
            dto::EntityKind::ProtoEnum => Self::ProtoEnum,
            dto::EntityKind::ProtoField => Self::ProtoField,
            dto::EntityKind::ProtoFile => Self::ProtoFile,
            dto::EntityKind::ProtoMessage => Self::ProtoMessage,
            dto::EntityKind::ProtoService => Self::ProtoService,
            dto::EntityKind::Rpc => Self::Rpc,
            dto::EntityKind::Service => Self::Service,
            dto::EntityKind::UnityPrefab => Self::UnityPrefab,
            dto::EntityKind::Unknown => Self::Unspecified,
        }
    }
}

impl From<dto::EntityOrigin> for v1::EntityOrigin {
    fn from(value: dto::EntityOrigin) -> Self {
        match value {
            dto::EntityOrigin::Source => Self::Source,
            dto::EntityOrigin::Generated => Self::Generated,
            dto::EntityOrigin::ExternalDependency => Self::ExternalDependency,
        }
    }
}

fn entity_origin(value: i32) -> Result<dto::EntityOrigin, &'static str> {
    match v1::EntityOrigin::try_from(value).map_err(|_| "unknown entity origin")? {
        v1::EntityOrigin::Unspecified => Err("entity origin is missing"),
        v1::EntityOrigin::Source => Ok(dto::EntityOrigin::Source),
        v1::EntityOrigin::Generated => Ok(dto::EntityOrigin::Generated),
        v1::EntityOrigin::ExternalDependency => Ok(dto::EntityOrigin::ExternalDependency),
    }
}

fn entity_kind(value: i32) -> Result<dto::EntityKind, &'static str> {
    Ok(
        match v1::EntityKind::try_from(value).map_err(|_| "unknown entity kind")? {
            v1::EntityKind::Callable => dto::EntityKind::Callable,
            v1::EntityKind::GraphqlArgument => dto::EntityKind::GraphqlArgument,
            v1::EntityKind::GraphqlEnumValue => dto::EntityKind::GraphqlEnumValue,
            v1::EntityKind::GraphqlField => dto::EntityKind::GraphqlField,
            v1::EntityKind::GraphqlOperation => dto::EntityKind::GraphqlOperation,
            v1::EntityKind::GraphqlType => dto::EntityKind::GraphqlType,
            v1::EntityKind::KafkaTopic => dto::EntityKind::KafkaTopic,
            v1::EntityKind::Namespace => dto::EntityKind::Namespace,
            v1::EntityKind::ProtoEnum => dto::EntityKind::ProtoEnum,
            v1::EntityKind::ProtoField => dto::EntityKind::ProtoField,
            v1::EntityKind::ProtoFile => dto::EntityKind::ProtoFile,
            v1::EntityKind::ProtoMessage => dto::EntityKind::ProtoMessage,
            v1::EntityKind::ProtoService => dto::EntityKind::ProtoService,
            v1::EntityKind::Rpc => dto::EntityKind::Rpc,
            v1::EntityKind::Service => dto::EntityKind::Service,
            v1::EntityKind::UnityPrefab => dto::EntityKind::UnityPrefab,
            v1::EntityKind::Unspecified => dto::EntityKind::Unknown,
        },
    )
}

fn entity_metadata(value: v1::EntityMetadata) -> Result<dto::EntityMetadata, &'static str> {
    match value.metadata.ok_or("entity metadata is missing")? {
        v1::entity_metadata::Metadata::GraphqlOperationKind(value) => {
            Ok(dto::EntityMetadata::GraphqlOperation {
                operation_kind: match v1::GraphqlOperationKind::try_from(value)
                    .map_err(|_| "unknown GraphQL operation kind")?
                {
                    v1::GraphqlOperationKind::Mutation => dto::GraphqlOperationKind::Mutation,
                    v1::GraphqlOperationKind::Query => dto::GraphqlOperationKind::Query,
                    v1::GraphqlOperationKind::Subscription => {
                        dto::GraphqlOperationKind::Subscription
                    }
                    v1::GraphqlOperationKind::Unspecified => {
                        return Err("GraphQL operation kind is missing");
                    }
                },
            })
        }
        v1::entity_metadata::Metadata::GraphqlTypeKind(value) => {
            Ok(dto::EntityMetadata::GraphqlType {
                type_kind: match v1::GraphqlTypeKind::try_from(value)
                    .map_err(|_| "unknown GraphQL type kind")?
                {
                    v1::GraphqlTypeKind::Enum => dto::GraphqlTypeKind::Enum,
                    v1::GraphqlTypeKind::Input => dto::GraphqlTypeKind::Input,
                    v1::GraphqlTypeKind::Interface => dto::GraphqlTypeKind::Interface,
                    v1::GraphqlTypeKind::Object => dto::GraphqlTypeKind::Object,
                    v1::GraphqlTypeKind::Scalar => dto::GraphqlTypeKind::Scalar,
                    v1::GraphqlTypeKind::Union => dto::GraphqlTypeKind::Union,
                    v1::GraphqlTypeKind::Unspecified => {
                        return Err("GraphQL type kind is missing");
                    }
                },
            })
        }
        v1::entity_metadata::Metadata::ProtoTypeKind(value) => Ok(dto::EntityMetadata::ProtoType {
            type_kind: match v1::ProtoTypeKind::try_from(value)
                .map_err(|_| "unknown Protobuf type kind")?
            {
                v1::ProtoTypeKind::Enum => dto::ProtoTypeKind::Enum,
                v1::ProtoTypeKind::Message => dto::ProtoTypeKind::Message,
                v1::ProtoTypeKind::Unspecified => return Err("Protobuf type kind is missing"),
            },
        }),
        v1::entity_metadata::Metadata::RpcCardinality(value) => {
            Ok(dto::EntityMetadata::ProtoMethod {
                cardinality: match v1::RpcCardinality::try_from(value)
                    .map_err(|_| "unknown RPC cardinality")?
                {
                    v1::RpcCardinality::BidirectionalStreaming => {
                        dto::RpcCardinality::BidirectionalStreaming
                    }
                    v1::RpcCardinality::ClientStreaming => dto::RpcCardinality::ClientStreaming,
                    v1::RpcCardinality::ServerStreaming => dto::RpcCardinality::ServerStreaming,
                    v1::RpcCardinality::Unary => dto::RpcCardinality::Unary,
                    v1::RpcCardinality::Unspecified => return Err("RPC cardinality is missing"),
                },
            })
        }
    }
}

fn protocol_entity_metadata(value: dto::EntityMetadata) -> v1::EntityMetadata {
    let metadata = match value {
        dto::EntityMetadata::GraphqlOperation { operation_kind } => {
            v1::entity_metadata::Metadata::GraphqlOperationKind(match operation_kind {
                dto::GraphqlOperationKind::Mutation => v1::GraphqlOperationKind::Mutation as i32,
                dto::GraphqlOperationKind::Query => v1::GraphqlOperationKind::Query as i32,
                dto::GraphqlOperationKind::Subscription => {
                    v1::GraphqlOperationKind::Subscription as i32
                }
            })
        }
        dto::EntityMetadata::GraphqlType { type_kind } => {
            v1::entity_metadata::Metadata::GraphqlTypeKind(match type_kind {
                dto::GraphqlTypeKind::Enum => v1::GraphqlTypeKind::Enum as i32,
                dto::GraphqlTypeKind::Input => v1::GraphqlTypeKind::Input as i32,
                dto::GraphqlTypeKind::Interface => v1::GraphqlTypeKind::Interface as i32,
                dto::GraphqlTypeKind::Object => v1::GraphqlTypeKind::Object as i32,
                dto::GraphqlTypeKind::Scalar => v1::GraphqlTypeKind::Scalar as i32,
                dto::GraphqlTypeKind::Union => v1::GraphqlTypeKind::Union as i32,
            })
        }
        dto::EntityMetadata::ProtoType { type_kind } => {
            v1::entity_metadata::Metadata::ProtoTypeKind(match type_kind {
                dto::ProtoTypeKind::Enum => v1::ProtoTypeKind::Enum as i32,
                dto::ProtoTypeKind::Message => v1::ProtoTypeKind::Message as i32,
            })
        }
        dto::EntityMetadata::ProtoMethod { cardinality } => {
            v1::entity_metadata::Metadata::RpcCardinality(match cardinality {
                dto::RpcCardinality::BidirectionalStreaming => {
                    v1::RpcCardinality::BidirectionalStreaming as i32
                }
                dto::RpcCardinality::ClientStreaming => v1::RpcCardinality::ClientStreaming as i32,
                dto::RpcCardinality::ServerStreaming => v1::RpcCardinality::ServerStreaming as i32,
                dto::RpcCardinality::Unary => v1::RpcCardinality::Unary as i32,
            })
        }
    };
    v1::EntityMetadata {
        metadata: Some(metadata),
    }
}

impl From<dto::EntityRef> for v1::Entity {
    fn from(value: dto::EntityRef) -> Self {
        Self {
            id: value.id,
            kind: v1::EntityKind::from(value.kind) as i32,
            name: value.name,
            repository: value.repository,
            origin: v1::EntityOrigin::from(value.origin) as i32,
            test: value.test,
            metadata: value.metadata.map(protocol_entity_metadata),
        }
    }
}

impl TryFrom<v1::Entity> for dto::EntityRef {
    type Error = &'static str;

    fn try_from(value: v1::Entity) -> Result<Self, Self::Error> {
        Ok(Self {
            id: value.id,
            kind: entity_kind(value.kind)?,
            name: value.name,
            repository: value.repository,
            origin: entity_origin(value.origin)?,
            test: value.test,
            metadata: value.metadata.map(entity_metadata).transpose()?,
        })
    }
}

impl From<dto::EvidenceKind> for v1::EvidenceKind {
    fn from(value: dto::EvidenceKind) -> Self {
        match value {
            dto::EvidenceKind::Ast => Self::Ast,
            dto::EvidenceKind::Compiler => Self::Compiler,
            dto::EvidenceKind::Configuration => Self::Configuration,
            dto::EvidenceKind::Descriptor => Self::Descriptor,
            dto::EvidenceKind::Generated => Self::Generated,
            dto::EvidenceKind::Inference => Self::Inference,
            dto::EvidenceKind::Unknown => Self::Unspecified,
        }
    }
}

fn evidence_kind(value: i32) -> Result<dto::EvidenceKind, &'static str> {
    Ok(
        match v1::EvidenceKind::try_from(value).map_err(|_| "unknown evidence kind")? {
            v1::EvidenceKind::Ast => dto::EvidenceKind::Ast,
            v1::EvidenceKind::Compiler => dto::EvidenceKind::Compiler,
            v1::EvidenceKind::Configuration => dto::EvidenceKind::Configuration,
            v1::EvidenceKind::Descriptor => dto::EvidenceKind::Descriptor,
            v1::EvidenceKind::Generated => dto::EvidenceKind::Generated,
            v1::EvidenceKind::Inference => dto::EvidenceKind::Inference,
            v1::EvidenceKind::Unspecified => dto::EvidenceKind::Unknown,
        },
    )
}

impl From<dto::EvidenceRef> for v1::Evidence {
    fn from(value: dto::EvidenceRef) -> Self {
        Self {
            source: v1::EvidenceKind::from(value.source_kind) as i32,
            repository: value.repository,
            path: value.path,
            line: value.line,
            detail: value.detail,
            range: value.range.map(Into::into),
            contexts: value.contexts.into_iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<v1::Evidence> for dto::EvidenceRef {
    type Error = &'static str;

    fn try_from(value: v1::Evidence) -> Result<Self, Self::Error> {
        Ok(Self {
            source_kind: evidence_kind(value.source)?,
            repository: value.repository,
            path: value.path,
            line: value.line,
            range: value.range.map(TryInto::try_into).transpose()?,
            detail: value.detail,
            contexts: value
                .contexts
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
        })
    }
}

impl From<dto::SourcePosition> for v1::SourcePosition {
    fn from(value: dto::SourcePosition) -> Self {
        Self {
            line: value.line,
            character: value.character,
        }
    }
}

impl From<v1::SourcePosition> for dto::SourcePosition {
    fn from(value: v1::SourcePosition) -> Self {
        Self {
            line: value.line,
            character: value.character,
        }
    }
}

impl From<dto::SourceRange> for v1::SourceRange {
    fn from(value: dto::SourceRange) -> Self {
        Self {
            start: Some(value.start.into()),
            end: Some(value.end.into()),
        }
    }
}

impl TryFrom<v1::SourceRange> for dto::SourceRange {
    type Error = &'static str;

    fn try_from(value: v1::SourceRange) -> Result<Self, Self::Error> {
        let range = Self {
            start: value.start.ok_or("source range start is missing")?.into(),
            end: value.end.ok_or("source range end is missing")?.into(),
        };
        (range.start <= range.end)
            .then_some(range)
            .ok_or("source range end precedes its start")
    }
}

impl From<dto::SourceExcerpt> for v1::SourceExcerpt {
    fn from(value: dto::SourceExcerpt) -> Self {
        Self {
            text: value.text,
            range: Some(value.range.into()),
        }
    }
}

impl TryFrom<v1::SourceExcerpt> for dto::SourceExcerpt {
    type Error = &'static str;

    fn try_from(value: v1::SourceExcerpt) -> Result<Self, Self::Error> {
        Ok(Self {
            text: value.text,
            range: value
                .range
                .ok_or("source excerpt range is missing")?
                .try_into()?,
        })
    }
}

impl From<dto::EvidenceContext> for v1::EvidenceContext {
    fn from(value: dto::EvidenceContext) -> Self {
        use v1::evidence_context::Context;
        Self {
            context: Some(match value {
                dto::EvidenceContext::ConditionArm {
                    construct,
                    arm,
                    condition,
                    arm_range,
                } => Context::ConditionArm(v1::ConditionArmContext {
                    construct: match construct {
                        dto::ConditionConstruct::If => v1::ConditionConstruct::If,
                        dto::ConditionConstruct::Cond => v1::ConditionConstruct::Cond,
                        dto::ConditionConstruct::Ternary => v1::ConditionConstruct::Ternary,
                        dto::ConditionConstruct::TemplateIf => v1::ConditionConstruct::TemplateIf,
                    } as i32,
                    arm: match arm {
                        dto::ConditionArmKind::Then => v1::ConditionArmKind::Then,
                        dto::ConditionArmKind::ElseIf => v1::ConditionArmKind::ElseIf,
                        dto::ConditionArmKind::Else => v1::ConditionArmKind::Else,
                        dto::ConditionArmKind::Clause => v1::ConditionArmKind::Clause,
                        dto::ConditionArmKind::Consequence => v1::ConditionArmKind::Consequence,
                        dto::ConditionArmKind::Alternative => v1::ConditionArmKind::Alternative,
                    } as i32,
                    condition: condition.map(Into::into),
                    arm_range: Some(arm_range.into()),
                }),
                dto::EvidenceContext::PatternArm {
                    construct,
                    selector,
                    pattern,
                    guard,
                    is_default,
                    arm_range,
                } => Context::PatternArm(v1::PatternArmContext {
                    construct: match construct {
                        dto::PatternConstruct::Match => v1::PatternConstruct::Match,
                        dto::PatternConstruct::Case => v1::PatternConstruct::Case,
                        dto::PatternConstruct::SwitchStatement => {
                            v1::PatternConstruct::SwitchStatement
                        }
                        dto::PatternConstruct::SwitchExpression => {
                            v1::PatternConstruct::SwitchExpression
                        }
                    } as i32,
                    selector: selector.map(Into::into),
                    pattern: pattern.map(Into::into),
                    guard: guard.map(Into::into),
                    is_default,
                    arm_range: Some(arm_range.into()),
                }),
                dto::EvidenceContext::CallableClause {
                    role,
                    signature,
                    guard,
                    definition_range,
                } => Context::CallableClause(v1::CallableClauseContext {
                    role: match role {
                        dto::CallableClauseRole::Declaration => v1::CallableClauseRole::Declaration,
                        dto::CallableClauseRole::Enclosing => v1::CallableClauseRole::Enclosing,
                        dto::CallableClauseRole::SelectedTarget => {
                            v1::CallableClauseRole::SelectedTarget
                        }
                    } as i32,
                    signature: Some(signature.into()),
                    guard: guard.map(Into::into),
                    definition_range: Some(definition_range.into()),
                }),
            }),
        }
    }
}

impl TryFrom<v1::EvidenceContext> for dto::EvidenceContext {
    type Error = &'static str;

    fn try_from(value: v1::EvidenceContext) -> Result<Self, Self::Error> {
        use v1::evidence_context::Context;
        Ok(
            match value.context.ok_or("evidence context kind is missing")? {
                Context::ConditionArm(value) => Self::ConditionArm {
                    construct: match v1::ConditionConstruct::try_from(value.construct)
                        .map_err(|_| "condition construct is unknown")?
                    {
                        v1::ConditionConstruct::Unspecified => {
                            return Err("condition construct is missing");
                        }
                        v1::ConditionConstruct::If => dto::ConditionConstruct::If,
                        v1::ConditionConstruct::Cond => dto::ConditionConstruct::Cond,
                        v1::ConditionConstruct::Ternary => dto::ConditionConstruct::Ternary,
                        v1::ConditionConstruct::TemplateIf => dto::ConditionConstruct::TemplateIf,
                    },
                    arm: match v1::ConditionArmKind::try_from(value.arm)
                        .map_err(|_| "condition arm kind is unknown")?
                    {
                        v1::ConditionArmKind::Unspecified => {
                            return Err("condition arm kind is missing");
                        }
                        v1::ConditionArmKind::Then => dto::ConditionArmKind::Then,
                        v1::ConditionArmKind::ElseIf => dto::ConditionArmKind::ElseIf,
                        v1::ConditionArmKind::Else => dto::ConditionArmKind::Else,
                        v1::ConditionArmKind::Clause => dto::ConditionArmKind::Clause,
                        v1::ConditionArmKind::Consequence => dto::ConditionArmKind::Consequence,
                        v1::ConditionArmKind::Alternative => dto::ConditionArmKind::Alternative,
                    },
                    condition: value.condition.map(TryInto::try_into).transpose()?,
                    arm_range: value
                        .arm_range
                        .ok_or("condition arm range is missing")?
                        .try_into()?,
                },
                Context::PatternArm(value) => Self::PatternArm {
                    construct: match v1::PatternConstruct::try_from(value.construct)
                        .map_err(|_| "pattern construct is unknown")?
                    {
                        v1::PatternConstruct::Unspecified => {
                            return Err("pattern construct is missing");
                        }
                        v1::PatternConstruct::Match => dto::PatternConstruct::Match,
                        v1::PatternConstruct::Case => dto::PatternConstruct::Case,
                        v1::PatternConstruct::SwitchStatement => {
                            dto::PatternConstruct::SwitchStatement
                        }
                        v1::PatternConstruct::SwitchExpression => {
                            dto::PatternConstruct::SwitchExpression
                        }
                    },
                    selector: value.selector.map(TryInto::try_into).transpose()?,
                    pattern: value.pattern.map(TryInto::try_into).transpose()?,
                    guard: value.guard.map(TryInto::try_into).transpose()?,
                    is_default: value.is_default,
                    arm_range: value
                        .arm_range
                        .ok_or("pattern arm range is missing")?
                        .try_into()?,
                },
                Context::CallableClause(value) => Self::CallableClause {
                    role: match v1::CallableClauseRole::try_from(value.role)
                        .map_err(|_| "callable clause role is unknown")?
                    {
                        v1::CallableClauseRole::Unspecified => {
                            return Err("callable clause role is missing");
                        }
                        v1::CallableClauseRole::Declaration => dto::CallableClauseRole::Declaration,
                        v1::CallableClauseRole::Enclosing => dto::CallableClauseRole::Enclosing,
                        v1::CallableClauseRole::SelectedTarget => {
                            dto::CallableClauseRole::SelectedTarget
                        }
                    },
                    signature: value
                        .signature
                        .ok_or("callable clause signature is missing")?
                        .try_into()?,
                    guard: value.guard.map(TryInto::try_into).transpose()?,
                    definition_range: value
                        .definition_range
                        .ok_or("callable clause definition range is missing")?
                        .try_into()?,
                },
            },
        )
    }
}

impl From<dto::RelationKind> for v1::RelationKind {
    fn from(value: dto::RelationKind) -> Self {
        match value {
            dto::RelationKind::BindsContract => Self::BindsContract,
            dto::RelationKind::Calls => Self::Calls,
            dto::RelationKind::CallsGraphql => Self::CallsGraphql,
            dto::RelationKind::CallsRpc => Self::CallsRpc,
            dto::RelationKind::ConsumedBy => Self::ConsumedBy,
            dto::RelationKind::Defines => Self::Defines,
            dto::RelationKind::FieldOf => Self::FieldOf,
            dto::RelationKind::Implements => Self::Implements,
            dto::RelationKind::ImplementedBy => Self::ImplementedBy,
            dto::RelationKind::Imports => Self::Imports,
            dto::RelationKind::Publishes => Self::Publishes,
            dto::RelationKind::Requires => Self::Requires,
            dto::RelationKind::RequestType => Self::RequestType,
            dto::RelationKind::ResolvedBy => Self::ResolvedBy,
            dto::RelationKind::Selects => Self::Selects,
            dto::RelationKind::ResponseType => Self::ResponseType,
            dto::RelationKind::Uses => Self::Uses,
        }
    }
}

pub(super) fn relation_kind(value: i32) -> Result<dto::RelationKind, &'static str> {
    match v1::RelationKind::try_from(value).map_err(|_| "unknown relation kind")? {
        v1::RelationKind::BindsContract => Ok(dto::RelationKind::BindsContract),
        v1::RelationKind::Calls => Ok(dto::RelationKind::Calls),
        v1::RelationKind::CallsGraphql => Ok(dto::RelationKind::CallsGraphql),
        v1::RelationKind::CallsRpc => Ok(dto::RelationKind::CallsRpc),
        v1::RelationKind::ConsumedBy => Ok(dto::RelationKind::ConsumedBy),
        v1::RelationKind::Defines => Ok(dto::RelationKind::Defines),
        v1::RelationKind::FieldOf => Ok(dto::RelationKind::FieldOf),
        v1::RelationKind::Implements => Ok(dto::RelationKind::Implements),
        v1::RelationKind::ImplementedBy => Ok(dto::RelationKind::ImplementedBy),
        v1::RelationKind::Imports => Ok(dto::RelationKind::Imports),
        v1::RelationKind::Publishes => Ok(dto::RelationKind::Publishes),
        v1::RelationKind::Requires => Ok(dto::RelationKind::Requires),
        v1::RelationKind::RequestType => Ok(dto::RelationKind::RequestType),
        v1::RelationKind::ResolvedBy => Ok(dto::RelationKind::ResolvedBy),
        v1::RelationKind::Selects => Ok(dto::RelationKind::Selects),
        v1::RelationKind::ResponseType => Ok(dto::RelationKind::ResponseType),
        v1::RelationKind::Uses => Ok(dto::RelationKind::Uses),
        v1::RelationKind::Unspecified => Err("relation kind is missing"),
    }
}

impl From<dto::SemanticEdge> for v1::Edge {
    fn from(value: dto::SemanticEdge) -> Self {
        Self {
            id: value.id,
            from: value.from,
            to: value.to,
            kind: v1::RelationKind::from(value.kind) as i32,
            confidence: value.confidence,
            evidence: value.evidence.into_iter().map(Into::into).collect(),
        }
    }
}

impl TryFrom<v1::Edge> for dto::SemanticEdge {
    type Error = &'static str;

    fn try_from(value: v1::Edge) -> Result<Self, Self::Error> {
        Ok(Self {
            id: value.id,
            from: value.from,
            to: value.to,
            kind: relation_kind(value.kind)?,
            confidence: value.confidence,
            evidence: value
                .evidence
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, _>>()?,
        })
    }
}

impl From<dto::SemanticPath> for v1::SemanticPath {
    fn from(value: dto::SemanticPath) -> Self {
        Self {
            nodes: value.nodes,
            edges: value.edges,
        }
    }
}

impl From<v1::SemanticPath> for dto::SemanticPath {
    fn from(value: v1::SemanticPath) -> Self {
        Self {
            nodes: value.nodes,
            edges: value.edges,
        }
    }
}
