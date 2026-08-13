pub mod v1 {
    tonic::include_proto!("beholder.v1");
}

use beholder_domain::Workspace as DomainWorkspace;
use beholder_dto::{QueryResult as DtoResult, QueryValue as DtoValue};
use std::path::PathBuf;
use v1::{QueryList, QueryResult, QueryRow, QueryValue, query_value};

impl From<DomainWorkspace> for v1::Workspace {
    fn from(workspace: DomainWorkspace) -> Self {
        Self {
            name: workspace.name,
            repositories: workspace
                .repositories
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
        }
    }
}

impl TryFrom<v1::Workspace> for DomainWorkspace {
    type Error = String;

    fn try_from(workspace: v1::Workspace) -> Result<Self, Self::Error> {
        Self::new(
            workspace.name,
            workspace
                .repositories
                .into_iter()
                .map(PathBuf::from)
                .collect(),
        )
    }
}

impl From<DtoResult> for QueryResult {
    fn from(result: DtoResult) -> Self {
        Self {
            headers: result.headers,
            rows: result
                .rows
                .into_iter()
                .map(|values| QueryRow {
                    values: values.into_iter().map(Into::into).collect(),
                })
                .collect(),
            next: result.next.map(|next| Box::new((*next).into())),
        }
    }
}

impl TryFrom<QueryResult> for DtoResult {
    type Error = &'static str;

    fn try_from(result: QueryResult) -> Result<Self, Self::Error> {
        Ok(Self {
            headers: result.headers,
            rows: result
                .rows
                .into_iter()
                .map(|row| row.values.into_iter().map(TryInto::try_into).collect())
                .collect::<Result<_, _>>()?,
            next: match result.next {
                Some(next) => Some(Box::new((*next).try_into()?)),
                None => None,
            },
        })
    }
}

impl From<DtoValue> for QueryValue {
    fn from(value: DtoValue) -> Self {
        let value = match value {
            DtoValue::Null => query_value::Value::Null(true),
            DtoValue::Boolean(value) => query_value::Value::Boolean(value),
            DtoValue::Integer(value) => query_value::Value::Integer(value),
            DtoValue::Float(value) => query_value::Value::Float(value),
            DtoValue::String(value) => query_value::Value::Text(value),
            DtoValue::Bytes(value) => query_value::Value::Bytes(value),
            DtoValue::List(values) => query_value::Value::List(QueryList {
                values: values.into_iter().map(Into::into).collect(),
            }),
            DtoValue::Other(value) => query_value::Value::Other(value),
        };
        Self { value: Some(value) }
    }
}

impl TryFrom<QueryValue> for DtoValue {
    type Error = &'static str;

    fn try_from(value: QueryValue) -> Result<Self, Self::Error> {
        Ok(
            match value.value.ok_or("query value is missing its value")? {
                query_value::Value::Null(_) => Self::Null,
                query_value::Value::Boolean(value) => Self::Boolean(value),
                query_value::Value::Integer(value) => Self::Integer(value),
                query_value::Value::Float(value) => Self::Float(value),
                query_value::Value::Text(value) => Self::String(value),
                query_value::Value::Bytes(value) => Self::Bytes(value),
                query_value::Value::List(values) => Self::List(
                    values
                        .values
                        .into_iter()
                        .map(TryInto::try_into)
                        .collect::<Result<_, _>>()?,
                ),
                query_value::Value::Other(value) => Self::Other(value),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_smoke() {
        let workspace = DomainWorkspace::new("main", vec![PathBuf::from("/tmp/repo")]).unwrap();
        assert_eq!(
            DomainWorkspace::try_from(v1::Workspace::from(workspace.clone())).unwrap(),
            workspace
        );
        let result = DtoResult {
            headers: vec!["value".into()],
            rows: vec![vec![DtoValue::List(vec![
                DtoValue::Null,
                DtoValue::Boolean(true),
                DtoValue::Integer(42),
                DtoValue::Float(1.5),
                DtoValue::String("text".into()),
                DtoValue::Bytes(vec![1, 2]),
                DtoValue::Other("other".into()),
            ])]],
            next: None,
        };
        assert_eq!(
            DtoResult::try_from(QueryResult::from(result.clone())).unwrap(),
            result
        );
        assert!(DtoValue::try_from(QueryValue { value: None }).is_err());
    }
}
