use beholder_adapters_protobuf::ProtobufAnalyzer;
use beholder_adapters_treesitter_csharp::CsharpAnalyzer;
use beholder_adapters_treesitter_elixir::ElixirAnalyzer;
use beholder_adapters_treesitter_rust::RustAnalyzer;
use beholder_adapters_treesitter_typescript::TypescriptAnalyzer;
use beholder_domain::{LogicalRepository, RepositoryDependencyGraph, RepositoryState};
use beholder_indexing::{
    AnalysisInputKind, Indexer, IndexerBuilder, InputKind, RepositoryInput, RepositorySnapshot,
    WorkspaceAnalyzer, WorkspaceSnapshot,
};
use beholder_worker_client::WorkerAnalyzerBuilder;
use std::{collections::BTreeMap, path::Path, path::PathBuf, sync::Arc};

#[derive(Clone, Copy)]
struct RoleFixture {
    path: &'static str,
    kind: AnalysisInputKind,
    input_kind: InputKind,
}

struct LanguageFixture {
    analyzer: &'static str,
    repository: &'static str,
    roles: &'static [RoleFixture],
}

const RUST_ROLES: &[RoleFixture] = &[
    role("src/lib.rs", AnalysisInputKind::Source),
    role("Cargo.toml", AnalysisInputKind::Dependency),
    role(".cargo/config.toml", AnalysisInputKind::Configuration),
    role("rust-toolchain.toml", AnalysisInputKind::Toolchain),
];
const ELIXIR_ROLES: &[RoleFixture] = &[
    role("lib/app.ex", AnalysisInputKind::Source),
    role("mix.lock", AnalysisInputKind::Dependency),
    role("config/config.exs", AnalysisInputKind::Configuration),
];
const TYPESCRIPT_ROLES: &[RoleFixture] = &[
    role("src/index.ts", AnalysisInputKind::Source),
    role("package-lock.json", AnalysisInputKind::Dependency),
    role("tsconfig.json", AnalysisInputKind::Configuration),
];
const CSHARP_ROLES: &[RoleFixture] = &[
    role("Assets/App.cs", AnalysisInputKind::Source),
    role("App.csproj", AnalysisInputKind::Dependency),
    role("Directory.Build.props", AnalysisInputKind::Configuration),
    role("global.json", AnalysisInputKind::Toolchain),
];
const PROTOBUF_ROLES: &[RoleFixture] = &[
    role("proto/api.proto", AnalysisInputKind::Source),
    role("buf.lock", AnalysisInputKind::Dependency),
    role("buf.yaml", AnalysisInputKind::Configuration),
    RoleFixture {
        path: "descriptor/api.pb",
        kind: AnalysisInputKind::Source,
        input_kind: InputKind::ProtobufDescriptor,
    },
];

const LANGUAGE_FIXTURES: &[LanguageFixture] = &[
    LanguageFixture {
        analyzer: "rust",
        repository: "fixture/rust",
        roles: RUST_ROLES,
    },
    LanguageFixture {
        analyzer: "elixir",
        repository: "fixture/elixir",
        roles: ELIXIR_ROLES,
    },
    LanguageFixture {
        analyzer: "typescript",
        repository: "fixture/typescript",
        roles: TYPESCRIPT_ROLES,
    },
    LanguageFixture {
        analyzer: "csharp",
        repository: "fixture/csharp",
        roles: CSHARP_ROLES,
    },
    LanguageFixture {
        analyzer: "protobuf",
        repository: "fixture/protobuf",
        roles: PROTOBUF_ROLES,
    },
];

const fn role(path: &'static str, kind: AnalysisInputKind) -> RoleFixture {
    RoleFixture {
        path,
        kind,
        input_kind: InputKind::Source,
    }
}

fn baseline_indexer() -> Indexer {
    let cache = PathBuf::new();
    IndexerBuilder::new(cache.clone(), 1)
        .add_analyzer(RustAnalyzer::new(cache.clone()))
        .add_analyzer(ElixirAnalyzer::new(cache.clone()))
        .add_analyzer(TypescriptAnalyzer::new(cache.clone()))
        .add_analyzer(CsharpAnalyzer::new(cache.clone()))
        .add_analyzer(ProtobufAnalyzer::new(cache))
        .build()
        .expect("language analyzers should compose")
}

fn compiler_indexer(rust_environment: &[u8], elixir_environment: &[u8]) -> Indexer {
    let rust = WorkerAnalyzerBuilder::new("unused-rust-worker", "unused-sockets")
        .identity("rust", "conformance")
        .accept_extension("rs")
        .accept_file_name_as("Cargo.toml", AnalysisInputKind::Dependency)
        .accept_file_name_as("Cargo.lock", AnalysisInputKind::Dependency)
        .accept_file_name_as("rust-toolchain", AnalysisInputKind::Toolchain)
        .accept_file_name_as("rust-toolchain.toml", AnalysisInputKind::Toolchain)
        .accept_path_suffix_as(".cargo/config", AnalysisInputKind::Configuration)
        .accept_path_suffix_as(".cargo/config.toml", AnalysisInputKind::Configuration)
        .identity_input(
            "$environment/RUSTFLAGS",
            rust_environment,
            AnalysisInputKind::Environment,
        )
        .build()
        .expect("Rust worker fixture should be valid");
    let elixir = WorkerAnalyzerBuilder::new("unused-elixir-worker", "unused-sockets")
        .identity("elixir", "conformance")
        .accept_extension("ex")
        .accept_extension("exs")
        .accept_file_name_as("mix.exs", AnalysisInputKind::Dependency)
        .accept_file_name_as("mix.lock", AnalysisInputKind::Dependency)
        .accept_parent_suffix_as("config", AnalysisInputKind::Configuration)
        .accept_parent_suffix_as("priv", AnalysisInputKind::Configuration)
        .exclude_path_suffix("config/runtime.exs")
        .identity_input(
            "$environment/BEHOLDER_ELIXIR_MIX_ENV",
            elixir_environment,
            AnalysisInputKind::Environment,
        )
        .build()
        .expect("Elixir worker fixture should be valid");
    IndexerBuilder::new(PathBuf::new(), 1)
        .add_enricher(rust)
        .add_enricher(elixir)
        .build()
        .expect("compiler enrichers should compose")
}

fn language_snapshot() -> WorkspaceSnapshot {
    WorkspaceSnapshot {
        name: "semantic-input-conformance".into(),
        repositories: LANGUAGE_FIXTURES
            .iter()
            .map(|fixture| RepositorySnapshot {
                base: PathBuf::from("/workspace").join(fixture.analyzer),
                state: repository_state(fixture.repository),
                inputs: fixture
                    .roles
                    .iter()
                    .map(|input| RepositoryInput {
                        path: input.path.into(),
                        content: bytes(format!("{}:{}", fixture.analyzer, input.path)),
                        kind: input.input_kind,
                    })
                    .chain([RepositoryInput {
                        path: "README.md".into(),
                        content: bytes("irrelevant"),
                        kind: InputKind::Source,
                    }])
                    .collect(),
            })
            .collect(),
    }
}

fn repository_state(identity: &str) -> RepositoryState {
    RepositoryState {
        repository: LogicalRepository {
            identity: identity.into(),
        },
        head: None,
        fingerprint: format!("fingerprint:{identity}"),
    }
}

fn bytes(content: impl AsRef<[u8]>) -> Arc<[u8]> {
    Arc::from(content.as_ref())
}

fn mutate(snapshot: &WorkspaceSnapshot, repository: &str, path: &str) -> WorkspaceSnapshot {
    let mut changed = snapshot.clone();
    let repository = changed
        .repositories
        .iter_mut()
        .find(|candidate| candidate.state.repository.identity == repository)
        .expect("fixture repository should exist");
    let input = repository
        .inputs
        .iter_mut()
        .find(|input| input.path == Path::new(path))
        .expect("fixture input should exist");
    input.content = bytes(format!("changed:{path}"));
    repository.state.fingerprint = format!("changed:{}", repository.state.repository.identity);
    changed
}

fn assert_only_identity_changed(
    original: &BTreeMap<String, BTreeMap<String, String>>,
    changed: &BTreeMap<String, BTreeMap<String, String>>,
    analyzer: &str,
    repository: &str,
) {
    for (candidate_analyzer, repositories) in original {
        for (candidate_repository, identity) in repositories {
            let changed_identity = &changed[candidate_analyzer][candidate_repository];
            if candidate_analyzer == analyzer && candidate_repository == repository {
                assert_ne!(identity, changed_identity, "{analyzer}:{repository}");
            } else {
                assert_eq!(
                    identity, changed_identity,
                    "{candidate_analyzer}:{candidate_repository}"
                );
            }
        }
    }
}

#[test]
fn every_language_declares_the_shared_semantic_input_roles() {
    let cache = PathBuf::new();
    let analyzers: Vec<(&str, Box<dyn WorkspaceAnalyzer>)> = vec![
        ("rust", Box::new(RustAnalyzer::new(cache.clone()))),
        ("elixir", Box::new(ElixirAnalyzer::new(cache.clone()))),
        (
            "typescript",
            Box::new(TypescriptAnalyzer::new(cache.clone())),
        ),
        ("csharp", Box::new(CsharpAnalyzer::new(cache.clone()))),
        ("protobuf", Box::new(ProtobufAnalyzer::new(cache))),
    ];

    for fixture in LANGUAGE_FIXTURES {
        let analyzer = analyzers
            .iter()
            .find(|(id, _)| *id == fixture.analyzer)
            .map(|(_, analyzer)| analyzer)
            .expect("fixture analyzer should exist");
        for input in fixture
            .roles
            .iter()
            .filter(|input| input.input_kind == InputKind::Source)
        {
            assert_eq!(
                analyzer.analysis_input_kind(Path::new(input.path)),
                Some(input.kind),
                "{} should classify {}",
                fixture.analyzer,
                input.path
            );
        }
        assert_eq!(analyzer.analysis_input_kind(Path::new("README.md")), None);
    }
}

#[test]
fn baseline_identities_change_only_for_the_owning_language_and_target() {
    let indexer = baseline_indexer();
    let snapshot = language_snapshot();
    let original = indexer.analysis_input_identities(&snapshot);

    for fixture in LANGUAGE_FIXTURES {
        for input in fixture.roles {
            let changed = indexer.analysis_input_identities(&mutate(
                &snapshot,
                fixture.repository,
                input.path,
            ));
            assert_only_identity_changed(&original, &changed, fixture.analyzer, fixture.repository);
        }
    }

    for fixture in LANGUAGE_FIXTURES {
        assert_eq!(
            original,
            indexer.analysis_input_identities(&mutate(&snapshot, fixture.repository, "README.md",))
        );
    }
}

#[test]
fn unrelated_repositories_and_restart_preserve_existing_identities() {
    let snapshot = language_snapshot();
    let original = baseline_indexer().analysis_input_identities(&snapshot);
    assert_eq!(
        original,
        baseline_indexer().analysis_input_identities(&snapshot),
        "a fresh indexer must produce the same identities"
    );

    let mut with_unrelated = snapshot;
    with_unrelated.repositories.push(RepositorySnapshot {
        base: "/workspace/unrelated".into(),
        state: repository_state("fixture/unrelated"),
        inputs: vec![RepositoryInput {
            path: "README.md".into(),
            content: bytes("unrelated repository"),
            kind: InputKind::Source,
        }],
    });
    let extended = baseline_indexer().analysis_input_identities(&with_unrelated);
    for (analyzer, repositories) in original {
        for (repository, identity) in repositories {
            assert_eq!(identity, extended[&analyzer][&repository]);
        }
    }
}

#[test]
fn compiler_identities_scope_repository_and_environment_changes() {
    let snapshot = language_snapshot();
    let indexer = compiler_indexer(b"", b"dev");
    let original = indexer.enrichment_input_identities(&snapshot);

    for fixture in LANGUAGE_FIXTURES
        .iter()
        .filter(|fixture| matches!(fixture.analyzer, "rust" | "elixir"))
    {
        for input in fixture.roles {
            let changed = indexer.enrichment_input_identities(&mutate(
                &snapshot,
                fixture.repository,
                input.path,
            ));
            assert_only_identity_changed(&original, &changed, fixture.analyzer, fixture.repository);
        }
    }
    assert_eq!(
        original,
        indexer.enrichment_input_identities(&mutate(&snapshot, "fixture/rust", "README.md",))
    );
    assert_eq!(
        original,
        compiler_indexer(b"", b"dev").enrichment_input_identities(&snapshot)
    );

    let rust_environment =
        compiler_indexer(b"--cfg conformance", b"dev").enrichment_input_identities(&snapshot);
    let elixir_environment = compiler_indexer(b"", b"test").enrichment_input_identities(&snapshot);
    for analyzer in ["rust", "elixir"] {
        for repository in original[analyzer].keys() {
            if analyzer == "rust" {
                assert_ne!(
                    original[analyzer][repository],
                    rust_environment[analyzer][repository]
                );
                assert_eq!(
                    original[analyzer][repository],
                    elixir_environment[analyzer][repository]
                );
            } else {
                assert_eq!(
                    original[analyzer][repository],
                    rust_environment[analyzer][repository]
                );
                assert_ne!(
                    original[analyzer][repository],
                    elixir_environment[analyzer][repository]
                );
            }
        }
    }
}

fn dependency_snapshot() -> WorkspaceSnapshot {
    WorkspaceSnapshot {
        name: "dependency-conformance".into(),
        repositories: vec![
            repository(
                "app",
                "/workspace/app",
                &[
                    (
                        "Cargo.toml",
                        "[dependencies]\ncontext = { path = \"../rust-context\" }\n",
                    ),
                    (
                        "mix.exs",
                        "defp deps, do: [{:context, path: \"../elixir-context\"}]\n",
                    ),
                    (
                        "tsconfig.json",
                        r#"{ "references": [{ "path": "../typescript-context" }] }"#,
                    ),
                    (
                        "App.csproj",
                        r#"<Project><ItemGroup><ProjectReference Include="../csharp-context/Context.csproj"/></ItemGroup></Project>"#,
                    ),
                ],
            ),
            repository(
                "rust-context",
                "/workspace/rust-context",
                &[(
                    "Cargo.toml",
                    "[dependencies]\napp = { path = \"../app\" }\n",
                )],
            ),
            repository(
                "elixir-context",
                "/workspace/elixir-context",
                &[("mix.exs", "def project, do: []\n")],
            ),
            repository(
                "typescript-context",
                "/workspace/typescript-context",
                &[("package.json", r#"{ "name": "context" }"#)],
            ),
            repository(
                "csharp-context",
                "/workspace/csharp-context",
                &[("Context.csproj", "<Project/>")],
            ),
            repository(
                "unrelated",
                "/workspace/unrelated",
                &[("README.md", "irrelevant")],
            ),
        ],
    }
}

fn repository(identity: &str, base: &str, inputs: &[(&str, &str)]) -> RepositorySnapshot {
    RepositorySnapshot {
        base: base.into(),
        state: repository_state(identity),
        inputs: inputs
            .iter()
            .map(|(path, content)| RepositoryInput {
                path: (*path).into(),
                content: bytes(*content),
                kind: InputKind::Source,
            })
            .collect(),
    }
}

#[test]
fn dependency_evidence_is_deterministic_scoped_and_cycle_safe() {
    let indexer = baseline_indexer();
    let snapshot = dependency_snapshot();
    let dependencies = indexer
        .repository_dependencies(&snapshot)
        .expect("dependency fixtures should parse");
    let mut reordered = snapshot.clone();
    reordered.repositories.reverse();
    for repository in &mut reordered.repositories {
        repository.inputs.reverse();
    }
    assert_eq!(
        dependencies,
        indexer
            .repository_dependencies(&reordered)
            .expect("reordered fixtures should parse")
    );
    assert!(dependencies.iter().all(|dependency| {
        matches!(
            dependency.analyzer.as_str(),
            "rust" | "elixir" | "typescript" | "csharp"
        )
    }));

    let mut graph = RepositoryDependencyGraph::new(
        snapshot
            .repositories
            .iter()
            .map(|repository| repository.state.repository.identity.clone()),
    )
    .expect("fixture repositories should be valid");
    graph
        .add_candidates(dependencies)
        .expect("dependency evidence should reference fixture repositories");
    assert_eq!(
        graph.context_map_for("rust"),
        BTreeMap::from([
            ("app".into(), vec!["rust-context".into()]),
            ("rust-context".into(), vec!["app".into()]),
        ])
    );
    for (analyzer, context) in [
        ("elixir", "elixir-context"),
        ("typescript", "typescript-context"),
        ("csharp", "csharp-context"),
    ] {
        assert_eq!(
            graph.context_map_for(analyzer),
            BTreeMap::from([("app".into(), vec![context.into()])])
        );
    }
    assert!(graph.context_map_for("protobuf").is_empty());
    assert!(graph.context_map_for("unknown").is_empty());
}

#[test]
fn prepared_analysis_cannot_publish_after_a_snapshot_mutation() {
    let indexer = baseline_indexer();
    let original = WorkspaceSnapshot {
        name: "immutable-conformance".into(),
        repositories: vec![repository(
            "app",
            "/workspace/app",
            &[("src/lib.rs", "pub fn value() {}")],
        )],
    };
    let plan = indexer.prepare(&original);
    let mut changed = original.clone();
    changed.repositories[0].state.fingerprint = "mutated-during-analysis".into();

    let error = indexer
        .analyze_prepared(&changed, &plan)
        .err()
        .expect("stale prepared analysis should fail");
    assert_eq!(
        error.to_string(),
        "prepared analysis plan does not match workspace repositories"
    );
}
