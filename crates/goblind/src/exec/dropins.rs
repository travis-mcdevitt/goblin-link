//! Owner drop-in commands (port of `commands.py`).
//!
//! Each `<name>.json` in the commands directory declares one command named
//! after the file. `description`, `required` and `optional` are registered with
//! the Muse like the built-ins; `exec` runs as the run-as account with the
//! declared parameters as one JSON object on stdin. The directory is read once
//! at start; a file with a problem is logged and skipped and never stops the
//! daemon.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use serde_json::{json, Map, Value};

/// Default command timeout.
pub const DEFAULT_TIMEOUT_MS: i64 = 30_000;
/// Maximum command timeout.
pub const MAX_TIMEOUT_MS: i64 = 600_000;
/// The Muse waits this much longer than the command may run.
pub const TIMEOUT_GRACE_MS: i64 = 5_000;
const MAX_FILE_BYTES: u64 = 64 * 1024;
const MAX_NAME_LEN: usize = 64;
const RESERVED_PREFIXES: [&str; 4] = ["system.", "file.", "device.", "link."];
const PARAM_TYPES: [&str; 6] = ["string", "integer", "number", "boolean", "object", "array"];

/// A drop-in command definition that failed validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidCommand(pub String);

/// A validated owner command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropInCommand {
    /// Command name (`sensors.read`).
    pub name: String,
    /// Description shown to the Muse.
    pub description: String,
    /// Program and arguments (`exec`).
    pub argv: Vec<String>,
    /// Required parameters, by name.
    pub required: Map<String, Value>,
    /// Optional parameters, by name.
    pub optional: Map<String, Value>,
    /// Timeout in milliseconds (without the Muse grace).
    pub timeout_ms: i64,
}

impl DropInCommand {
    /// The `commands_v2` spec entry for `link.register`.
    pub fn spec(&self) -> Value {
        json!({
            "description": self.description,
            "required": self.required,
            "optional": self.optional,
            "timeout_ms": self.timeout_ms + TIMEOUT_GRACE_MS,
        })
    }

    /// The declared parameters from `params`, validated, or an error.
    pub fn check_params(
        &self,
        params: &Map<String, Value>,
    ) -> Result<Map<String, Value>, InvalidCommand> {
        for name in self.required.keys() {
            if !params.contains_key(name) {
                return Err(InvalidCommand(format!("{name} is required")));
            }
        }
        let mut checked = Map::new();
        for (name, spec) in self.required.iter().chain(self.optional.iter()) {
            let Some(value) = params.get(name) else {
                continue;
            };
            let ty = spec.get("type").and_then(Value::as_str).unwrap_or("");
            if !value_matches(value, ty) {
                let article = if "aeiou".contains(ty.chars().next().unwrap_or('x')) {
                    "an"
                } else {
                    "a"
                };
                return Err(InvalidCommand(format!("{name} must be {article} {ty}")));
            }
            checked.insert(name.clone(), value.clone());
        }
        Ok(checked)
    }
}

fn value_matches(value: &Value, ty: &str) -> bool {
    match ty {
        // bool is not an int in JSON.
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number() && !value.is_boolean(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        _ => false,
    }
}

fn name_ok(name: &str) -> bool {
    // lowercase words joined by dots, at least two components.
    if name.is_empty() {
        return false;
    }
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() < 2 {
        return false;
    }
    parts.iter().all(|p| {
        !p.is_empty()
            && p.chars()
                .next()
                .map(|c| c.is_ascii_lowercase())
                .unwrap_or(false)
            && p.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    })
}

fn params_table(value: &Value, key: &str) -> Result<Map<String, Value>, InvalidCommand> {
    let Some(obj) = value.as_object() else {
        return Err(InvalidCommand(format!(
            "{key} must be an object of parameters"
        )));
    };
    for (name, spec) in obj {
        let ok = spec.as_object().is_some_and(|s| {
            s.len() == 2
                && s.get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|t| PARAM_TYPES.contains(&t))
                && s.get("description").is_some_and(Value::is_string)
        });
        if !ok {
            return Err(InvalidCommand(format!(
                "{key}.{name} must be {{\"type\": ..., \"description\": ...}} with a valid type"
            )));
        }
    }
    Ok(obj.clone())
}

/// Validates one command file's contents.
pub fn parse(name: &str, data: &Value) -> Result<DropInCommand, InvalidCommand> {
    if name.len() > MAX_NAME_LEN || !name_ok(name) {
        return Err(InvalidCommand(
            "the file name must be the command name, like sensors.read.json".into(),
        ));
    }
    if RESERVED_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return Err(InvalidCommand(format!(
            "names starting with {} are reserved",
            RESERVED_PREFIXES.join(", ")
        )));
    }
    let Some(obj) = data.as_object() else {
        return Err(InvalidCommand("the file must hold a JSON object".into()));
    };
    let allowed: [&str; 5] = ["description", "exec", "required", "optional", "timeout_ms"];
    let unknown: Vec<&str> = obj
        .keys()
        .map(String::as_str)
        .filter(|k| !allowed.contains(k))
        .collect();
    if !unknown.is_empty() {
        let mut u = unknown;
        u.sort_unstable();
        return Err(InvalidCommand(format!("unknown keys: {}", u.join(", "))));
    }
    let description = obj.get("description").and_then(Value::as_str).unwrap_or("");
    if description.trim().is_empty() {
        return Err(InvalidCommand("description is required".into()));
    }
    let argv: Vec<String> = match obj.get("exec").and_then(Value::as_array) {
        Some(a) if !a.is_empty() && a.iter().all(|v| v.as_str().is_some_and(|s| !s.is_empty())) => {
            a.iter().map(|v| v.as_str().unwrap().to_owned()).collect()
        }
        _ => {
            return Err(InvalidCommand(
                "exec must be a list of strings: the program and its arguments".into(),
            ))
        }
    };
    if !Path::new(&argv[0]).is_absolute() {
        return Err(InvalidCommand(
            "exec must start with the program's absolute path".into(),
        ));
    }
    let required = params_table(obj.get("required").unwrap_or(&json!({})), "required")?;
    let optional = params_table(obj.get("optional").unwrap_or(&json!({})), "optional")?;
    let both: Vec<&String> = required
        .keys()
        .filter(|k| optional.contains_key(*k))
        .collect();
    if !both.is_empty() {
        let mut b: Vec<&str> = both.iter().map(|s| s.as_str()).collect();
        b.sort_unstable();
        return Err(InvalidCommand(format!(
            "both required and optional: {}",
            b.join(", ")
        )));
    }
    let timeout_ms = obj
        .get("timeout_ms")
        .map_or(DEFAULT_TIMEOUT_MS, |v| v.as_i64().unwrap_or(-1));
    if obj.get("timeout_ms").is_some_and(|v| v.is_boolean())
        || !(1..=MAX_TIMEOUT_MS).contains(&timeout_ms)
    {
        return Err(InvalidCommand(format!(
            "timeout_ms must be an integer from 1 to {MAX_TIMEOUT_MS}"
        )));
    }
    Ok(DropInCommand {
        name: name.to_owned(),
        description: description.to_owned(),
        argv,
        required,
        optional,
        timeout_ms,
    })
}

/// Loads the valid commands in `directory`, sorted by name. A missing directory
/// has none; problems with a file are logged and that file is skipped. When
/// running as root, the directory and files must be root-owned and not
/// group/other writable (like `sudoers.d`).
pub fn load(directory: &Path, is_root: bool) -> Vec<DropInCommand> {
    let entries = match std::fs::read_dir(directory) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            log::warn!(
                "can't read commands from {}: {:?}",
                directory.display(),
                e.kind()
            );
            return Vec::new();
        }
    };
    if is_root {
        if let Ok(meta) = std::fs::metadata(directory) {
            if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
                log::warn!(
                    "skipping commands directory {}: must be owned by root and writable only by root",
                    directory.display()
                );
                return Vec::new();
            }
        }
    }
    let mut files: BTreeMap<String, std::path::PathBuf> = BTreeMap::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.ends_with(".json") {
            files.insert(name.to_owned(), entry.path());
        }
    }
    let mut commands = Vec::new();
    for (file_name, path) in files {
        match load_file(&path, &file_name, is_root) {
            Ok(cmd) => commands.push(cmd),
            Err(e) => log::warn!("skipping {}: {}", path.display(), e),
        }
    }
    if !commands.is_empty() {
        let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
        log::info!(
            "commands from {}: {}",
            directory.display(),
            names.join(", ")
        );
    }
    commands
}

fn load_file(path: &Path, file_name: &str, is_root: bool) -> Result<DropInCommand, InvalidCommand> {
    let meta = std::fs::metadata(path)
        .map_err(|e| InvalidCommand(format!("can't read it: {:?}", e.kind())))?;
    if !meta.is_file() {
        return Err(InvalidCommand("not a regular file".into()));
    }
    if is_root && (meta.uid() != 0 || meta.mode() & 0o022 != 0) {
        return Err(InvalidCommand(
            "must be owned by root and writable only by root".into(),
        ));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(InvalidCommand(format!(
            "larger than {MAX_FILE_BYTES} bytes"
        )));
    }
    let raw = std::fs::read(path)
        .map_err(|e| InvalidCommand(format!("can't read it: {:?}", e.kind())))?;
    let data: Value =
        serde_json::from_slice(&raw).map_err(|_| InvalidCommand("not valid JSON".into()))?;
    let name = file_name.strip_suffix(".json").unwrap_or(file_name);
    parse(name, &data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_suggest_replies_command_is_valid() {
        let text =
            include_str!("../../../../packaging/nextui-pak/commands.d/chat.suggest_replies.json")
                .replace("@PAK_DIR@", "/mnt/SDCARD/Tools/tg5040/Goblin Link.pak");
        let data: Value = serde_json::from_str(&text).expect("json");
        let cmd = parse("chat.suggest_replies", &data).expect("valid drop-in");
        assert_eq!(cmd.argv[1..], ["chat", "suggest"]);
        assert!(cmd.description.contains("60 characters"));
        assert!(cmd.description.contains("owner speaking to you"));
    }

    #[test]
    fn parses_a_valid_command() {
        let data = json!({
            "description": "Switch the pump.",
            "exec": ["/usr/local/bin/pump"],
            "required": {"on": {"type": "boolean", "description": "true for on."}},
            "timeout_ms": 10000
        });
        let cmd = parse("pump.set", &data).unwrap();
        assert_eq!(cmd.name, "pump.set");
        assert_eq!(cmd.timeout_ms, 10000);
        assert_eq!(cmd.spec()["timeout_ms"], json!(15000));
    }

    #[test]
    fn rejects_reserved_prefixes_and_bad_names() {
        assert!(parse("system.run", &json!({"description":"x","exec":["/a"]})).is_err());
        assert!(parse("noplace", &json!({"description":"x","exec":["/a"]})).is_err());
        assert!(parse("Up.Case", &json!({"description":"x","exec":["/a"]})).is_err());
    }

    #[test]
    fn rejects_relative_exec_and_unknown_keys() {
        assert!(parse("a.b", &json!({"description":"x","exec":["rel"]})).is_err());
        assert!(parse("a.b", &json!({"description":"x","exec":["/a"],"nope":1})).is_err());
    }

    #[test]
    fn check_params_enforces_required_and_types() {
        let cmd = parse(
            "a.b",
            &json!({"description":"x","exec":["/a"],
                    "required":{"n":{"type":"integer","description":"d"}},
                    "optional":{"s":{"type":"string","description":"d"}}}),
        )
        .unwrap();
        assert!(cmd.check_params(&Map::new()).is_err());
        let mut p = Map::new();
        p.insert("n".into(), json!(true)); // bool is not an integer
        assert!(cmd.check_params(&p).is_err());
        let mut p = Map::new();
        p.insert("n".into(), json!(3));
        p.insert("s".into(), json!("hi"));
        assert_eq!(cmd.check_params(&p).unwrap().len(), 2);
    }
}
