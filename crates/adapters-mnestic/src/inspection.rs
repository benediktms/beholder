use mnestic_engine::{DataValue, NamedRows, Num};

#[derive(Clone, Debug, PartialEq)]
pub enum InspectionValue {
    Null,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    List(Vec<InspectionValue>),
    Other(String),
}

impl InspectionValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct InspectionResult {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<InspectionValue>>,
    pub next: Option<Box<InspectionResult>>,
}

pub(super) fn inspection_result(rows: NamedRows) -> InspectionResult {
    InspectionResult {
        headers: rows.headers,
        rows: rows
            .rows
            .into_iter()
            .map(|row| row.into_iter().map(inspection_value).collect())
            .collect(),
        next: rows.next.map(|next| Box::new(inspection_result(*next))),
    }
}

fn inspection_value(value: DataValue) -> InspectionValue {
    match value {
        DataValue::Null => InspectionValue::Null,
        DataValue::Bool(value) => InspectionValue::Boolean(value),
        DataValue::Num(Num::Int(value)) => InspectionValue::Integer(value),
        DataValue::Num(Num::Float(value)) => InspectionValue::Float(value),
        DataValue::Str(value) => InspectionValue::String(value.into()),
        DataValue::Bytes(value) => InspectionValue::Bytes(value),
        DataValue::List(values) => {
            InspectionValue::List(values.into_iter().map(inspection_value).collect())
        }
        value => InspectionValue::Other(value.to_string()),
    }
}
