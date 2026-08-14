use beholder_adapters_mnestic::{InspectionValue, explain};
use std::{env, error::Error, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let query_path = args.next().ok_or("usage: mnestic-plan <query-file>")?;
    if args.next().is_some() {
        return Err("usage: mnestic-plan <query-file>".into());
    }

    let plan = explain(&database_path(), &fs::read_to_string(query_path)?)?;
    println!("{}", plan.headers.join("\t"));
    for row in plan.rows {
        println!("{}", row.iter().map(render).collect::<Vec<_>>().join("\t"));
    }
    Ok(())
}

fn database_path() -> PathBuf {
    if let Some(path) = env::var_os("BEHOLDER_STATE_DIR").filter(|value| !value.is_empty()) {
        return PathBuf::from(path).join("daemon/beholder.db");
    }
    if let Some(path) = env::var_os("XDG_STATE_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(path).join("beholder/daemon/beholder.db");
    }
    PathBuf::from(env::var_os("HOME").expect("HOME, XDG_STATE_HOME, or BEHOLDER_STATE_DIR is set"))
        .join(".local/state/beholder/daemon/beholder.db")
}

fn render(value: &InspectionValue) -> String {
    match value {
        InspectionValue::Null => "null".into(),
        InspectionValue::Boolean(value) => value.to_string(),
        InspectionValue::Integer(value) => value.to_string(),
        InspectionValue::Float(value) => value.to_string(),
        InspectionValue::String(value) | InspectionValue::Other(value) => value.clone(),
        InspectionValue::Bytes(value) => format!("{value:?}"),
        InspectionValue::List(value) => format!("{value:?}"),
    }
}
