use beholder_adapters_git::repository_state_bytes;
use beholder_adapters_treesitter_typescript::SourceLanguage;
use beholder_domain::RepositoryState;
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
};

pub(super) type RustSources = Vec<(PathBuf, String)>;
pub(super) type ElixirSources = Vec<(PathBuf, String)>;
pub(super) type TypescriptSources = Vec<(PathBuf, String, SourceLanguage)>;
pub(super) type TypescriptManifests = Vec<(PathBuf, String)>;
pub(super) type TypescriptConfigs = Vec<(PathBuf, String)>;
pub(super) type ProtobufSources = Vec<(PathBuf, Vec<u8>)>;
pub(super) type ProtobufSourceFiles = Vec<(PathBuf, Vec<u8>)>;

#[derive(Debug)]
pub(super) struct RepositorySources {
    pub(super) state: RepositoryState,
    pub(super) rust: RustSources,
    pub(super) elixir: ElixirSources,
    pub(super) typescript: TypescriptSources,
    pub(super) typescript_manifests: TypescriptManifests,
    pub(super) typescript_configs: TypescriptConfigs,
    pub(super) protobuf: ProtobufSources,
    pub(super) protobuf_source: ProtobufSourceFiles,
}

pub(super) fn is_ignored_directory(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".next"
            | ".next-server"
            | ".turbo"
            | "_build"
            | "build"
            | "coverage"
            | "deps"
            | "dist"
            | "node_modules"
            | "storybook-static"
            | "target"
    )
}

pub(super) fn is_index_input(path: &Path) -> bool {
    !path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(is_ignored_directory)
    }) && (path.extension().is_some_and(|extension| {
        matches!(
            extension.to_str(),
            Some("rs" | "ex" | "exs" | "js" | "jsx" | "ts" | "tsx" | "proto")
        )
    }) || path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            matches!(name, "buf.yaml" | "buf.lock" | "package.json")
                || ((name.starts_with("tsconfig.") || name.starts_with("jsconfig."))
                    && name.ends_with(".json"))
        }))
}

fn source_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            if !entry.file_name().to_str().is_some_and(is_ignored_directory) {
                source_files(&path, files)?;
            }
        } else if is_index_input(&path) {
            files.push(path);
        }
    }
    Ok(())
}

pub(super) fn repository_sources(
    root: &Path,
    descriptor_paths: &[PathBuf],
) -> Result<RepositorySources, Box<dyn Error>> {
    if !root.is_dir() {
        return Err(format!("repository does not exist: {}", root.display()).into());
    }
    let mut files = Vec::new();
    source_files(root, &mut files)?;
    files.sort();
    let (typescript_manifest_files, files): (Vec<_>, Vec<_>) = files
        .into_iter()
        .partition(|path| path.file_name().and_then(|name| name.to_str()) == Some("package.json"));
    let typescript_manifests = typescript_manifest_files
        .into_iter()
        .map(|path| {
            Ok((
                path.strip_prefix(root)?.to_path_buf(),
                fs::read_to_string(path)?,
            ))
        })
        .collect::<Result<TypescriptManifests, Box<dyn Error>>>()?;
    let (typescript_config_files, files): (Vec<_>, Vec<_>) = files.into_iter().partition(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                (name.starts_with("tsconfig.") || name.starts_with("jsconfig."))
                    && name.ends_with(".json")
            })
    });
    let typescript_configs = typescript_config_files
        .into_iter()
        .map(|path| {
            Ok((
                path.strip_prefix(root)?.to_path_buf(),
                fs::read_to_string(path)?,
            ))
        })
        .collect::<Result<TypescriptConfigs, Box<dyn Error>>>()?;
    let (protobuf_files, sources): (Vec<_>, Vec<_>) = files.into_iter().partition(|path| {
        path.extension()
            .is_some_and(|extension| extension == "proto")
            || matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("buf.yaml" | "buf.lock")
            )
    });
    let (typescript_files, sources): (Vec<_>, Vec<_>) = sources
        .into_iter()
        .partition(|path| SourceLanguage::from_path(path).is_some());
    let typescript = typescript_files
        .into_iter()
        .map(|path| {
            let relative_path = path.strip_prefix(root)?.to_path_buf();
            let language =
                SourceLanguage::from_path(&relative_path).ok_or("missing JS/TS language")?;
            Ok((relative_path, fs::read_to_string(path)?, language))
        })
        .collect::<Result<TypescriptSources, Box<dyn Error>>>()?;
    let sources = sources
        .into_iter()
        .map(|path| {
            let relative_path = path.strip_prefix(root)?.to_path_buf();
            Ok((relative_path, fs::read_to_string(path)?))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    let (elixir, rust): (ElixirSources, RustSources) =
        sources.into_iter().partition(|(path, _)| {
            path.extension()
                .is_some_and(|extension| matches!(extension.to_str(), Some("ex" | "exs")))
        });
    let mut descriptors = descriptor_paths
        .iter()
        .map(|path| Ok((path.strip_prefix(root)?.to_path_buf(), fs::read(path)?)))
        .collect::<Result<ProtobufSources, Box<dyn Error>>>()?;
    descriptors.sort_by(|left, right| left.0.cmp(&right.0));
    let protobuf_source = protobuf_files
        .into_iter()
        .map(|path| Ok((path.strip_prefix(root)?.to_path_buf(), fs::read(path)?)))
        .collect::<Result<ProtobufSourceFiles, Box<dyn Error>>>()?;
    let state = repository_state_bytes(
        root,
        rust.iter()
            .map(|(path, source)| (path.as_path(), source.as_bytes()))
            .chain(
                elixir
                    .iter()
                    .map(|(path, source)| (path.as_path(), source.as_bytes())),
            )
            .chain(
                typescript
                    .iter()
                    .map(|(path, source, _)| (path.as_path(), source.as_bytes())),
            )
            .chain(
                typescript_manifests
                    .iter()
                    .map(|(path, source)| (path.as_path(), source.as_bytes())),
            )
            .chain(
                typescript_configs
                    .iter()
                    .map(|(path, source)| (path.as_path(), source.as_bytes())),
            )
            .chain(
                descriptors
                    .iter()
                    .map(|(path, bytes)| (path.as_path(), bytes.as_slice())),
            )
            .chain(
                protobuf_source
                    .iter()
                    .map(|(path, bytes)| (path.as_path(), bytes.as_slice())),
            ),
    )?;
    Ok(RepositorySources {
        state,
        rust,
        elixir,
        typescript,
        typescript_manifests,
        typescript_configs,
        protobuf: descriptors,
        protobuf_source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    #[test]
    fn rejects_missing_repository() {
        let error = repository_sources(Path::new("/definitely/missing"), &[]).unwrap_err();
        assert!(error.to_string().contains("repository does not exist"));
    }

    #[test]
    fn protobuf_inputs_participate_in_repository_state() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let repository = std::env::temp_dir().join(format!("beholder-protobuf-sources-{unique}"));
        fs::create_dir_all(&repository).unwrap();
        fs::write(repository.join("buf.yaml"), "version: v2\n").unwrap();
        fs::write(repository.join("buf.lock"), "version: v2\ndeps: []\n").unwrap();
        fs::write(
            repository.join("contract.proto"),
            "syntax = \"proto3\"; message Before {}",
        )
        .unwrap();
        fs::write(repository.join("README.md"), "ignored").unwrap();

        let before = repository_sources(&repository, &[]).unwrap();
        assert_eq!(before.protobuf_source.len(), 3);
        fs::write(
            repository.join("contract.proto"),
            "syntax = \"proto3\"; message After {}",
        )
        .unwrap();
        let after = repository_sources(&repository, &[]).unwrap();
        assert_ne!(before.state.fingerprint, after.state.fingerprint);
        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn discovers_javascript_and_typescript_but_skips_build_and_dependency_outputs() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let repository = std::env::temp_dir().join(format!("beholder-typescript-sources-{unique}"));
        for directory in [
            "src",
            "node_modules/package",
            "dist",
            ".next",
            ".next-server",
            "storybook-static",
        ] {
            fs::create_dir_all(repository.join(directory)).unwrap();
        }
        for path in ["src/a.js", "src/b.jsx", "src/c.ts", "src/d.tsx"] {
            fs::write(repository.join(path), "export function indexed() {}").unwrap();
        }
        fs::write(
            repository.join("package.json"),
            r#"{"name":"@example/app"}"#,
        )
        .unwrap();
        fs::write(
            repository.join("tsconfig.json"),
            "{ // comment\n \"compilerOptions\": { \"paths\": {}, },\n}",
        )
        .unwrap();
        for path in [
            "node_modules/package/index.ts",
            "dist/index.js",
            ".next/index.js",
            ".next-server/index.js",
            "storybook-static/index.js",
        ] {
            fs::write(repository.join(path), "export function ignored() {}").unwrap();
        }

        let sources = repository_sources(&repository, &[]).unwrap();
        assert_eq!(sources.typescript.len(), 4);
        assert_eq!(sources.typescript_manifests.len(), 1);
        assert_eq!(sources.typescript_configs.len(), 1);
        assert!(
            sources
                .typescript
                .iter()
                .all(|(path, _, _)| path.starts_with("src"))
        );
        fs::write(
            repository.join("package.json"),
            r#"{"name":"@example/renamed"}"#,
        )
        .unwrap();
        let renamed = repository_sources(&repository, &[]).unwrap();
        assert_ne!(sources.state.fingerprint, renamed.state.fingerprint);
        fs::remove_dir_all(repository).unwrap();
    }
}
