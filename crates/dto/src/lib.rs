#[derive(Clone, Debug, PartialEq)]
pub enum QueryValue {
    Null,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
    List(Vec<QueryValue>),
    Other(String),
}

impl QueryValue {
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
pub struct QueryResult {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<QueryValue>>,
    pub next: Option<Box<QueryResult>>,
}
