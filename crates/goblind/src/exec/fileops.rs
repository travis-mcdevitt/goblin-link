//! File reads and chunked atomic writes (port of `fileops.py`).
//!
//! Writes land in a hidden partial file next to the target and are published
//! atomically once the final chunk arrives and its SHA-256 matches. Unlike the
//! reference, these run in the daemon's own process rather than a dropped-
//! privilege child; see `docs/daemon.md` for that deviation.

use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

const MAX_CHUNK_BYTES: usize = 64 * 1024;

fn b64() -> base64::engine::general_purpose::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn partial_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(".{name}.musegadget-partial"))
}

fn require_absolute(params: &Map<String, Value>) -> Result<PathBuf, String> {
    let path = params.get("path").and_then(Value::as_str);
    match path {
        Some(p) if Path::new(p).is_absolute() => Ok(PathBuf::from(p)),
        _ => Err("path must be an absolute path".into()),
    }
}

/// Reads up to 64 KiB of a file, base64-encoded.
pub fn read(params: &Map<String, Value>) -> Result<Value, String> {
    let path = require_absolute(params)?;
    let offset = params.get("offset").and_then(Value::as_i64).unwrap_or(0);
    let mut limit = params
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(MAX_CHUNK_BYTES as i64);
    limit = limit.min(MAX_CHUNK_BYTES as i64);
    if offset < 0 || limit <= 0 {
        return Err("offset must be >= 0 and limit > 0".into());
    }
    let mut file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    file.seek(SeekFrom::Start(offset as u64))
        .map_err(|e| e.to_string())?;
    // Read limit+1 to find true EOF (procfs/sysfs report misleading sizes).
    let mut buf = vec![0u8; limit as usize + 1];
    let mut read_total = 0;
    while read_total < buf.len() {
        match file.read(&mut buf[read_total..]) {
            Ok(0) => break,
            Ok(n) => read_total += n,
            Err(e) => return Err(e.to_string()),
        }
    }
    let eof = read_total <= limit as usize;
    let data = &buf[..read_total.min(limit as usize)];
    let next_offset = offset as u64 + data.len() as u64;
    Ok(json!({
        "path": path.to_string_lossy(),
        "size": size,
        "offset": offset,
        "next_offset": next_offset,
        "eof": eof,
        "data_b64": b64().encode(data),
    }))
}

/// Writes a chunk of a file; publishes atomically on the final chunk.
pub fn write(params: &Map<String, Value>) -> Result<Value, String> {
    let path = require_absolute(params)?;
    let offset = params.get("offset").and_then(Value::as_i64).unwrap_or(0);
    if offset < 0 {
        return Err("offset must be >= 0".into());
    }
    let data = b64()
        .decode(params.get("data_b64").and_then(Value::as_str).unwrap_or(""))
        .map_err(|_| "data_b64 is not valid base64".to_string())?;
    if data.len() > MAX_CHUNK_BYTES {
        return Err(format!("chunk larger than {MAX_CHUNK_BYTES} bytes"));
    }
    let partial = partial_path(&path);

    let mut file = if offset == 0 {
        if params
            .get("create_parents")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
        }
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&partial)
            .map_err(|e| e.to_string())?;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        f
    } else {
        std::fs::OpenOptions::new()
            .write(true)
            .open(&partial)
            .map_err(|_| "no write in progress; start at offset 0".to_string())?
    };
    let size = file.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    if offset as u64 != size {
        return Err(format!("expected offset {size}, got {offset}"));
    }
    file.write_all(&data).map_err(|e| e.to_string())?;
    let written = file.stream_position().map_err(|e| e.to_string())?;
    drop(file);

    if !params
        .get("final")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(json!({"path": path.to_string_lossy(), "received": written, "complete": false}));
    }

    // Verify the whole-file hash.
    let mut digest = Sha256::new();
    let mut f = std::fs::File::open(&partial).map_err(|e| e.to_string())?;
    let mut buf = [0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
    }
    let hex = hex_digest(&digest.finalize());
    if let Some(expected) = params.get("sha256").and_then(Value::as_str) {
        if expected.to_ascii_lowercase() != hex {
            let _ = std::fs::remove_file(&partial);
            return Err("sha256 mismatch; write discarded".into());
        }
    }
    // Keep the replaced file's permission bits, else 0644 minus umask (0022).
    let mode = std::fs::metadata(&path)
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0o644 & !0o022);
    std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(mode))
        .map_err(|e| e.to_string())?;

    let overwrite = params
        .get("overwrite")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if overwrite {
        std::fs::rename(&partial, &path).map_err(|e| e.to_string())?;
    } else {
        if path.exists() {
            let _ = std::fs::remove_file(&partial);
            return Err("file exists and overwrite is false".into());
        }
        // Not strictly atomic without link(), but refuses an existing entry.
        std::fs::rename(&partial, &path).map_err(|e| e.to_string())?;
    }
    Ok(json!({
        "path": path.to_string_lossy(),
        "size": written,
        "sha256": hex,
        "complete": true,
    }))
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn write_then_read_round_trip_atomic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("out.bin");
        let content = b"hello world";
        let mut p = Map::new();
        p.insert("path".into(), json!(path.to_string_lossy()));
        p.insert("data_b64".into(), json!(b64().encode(content)));
        p.insert("final".into(), json!(true));
        p.insert("sha256".into(), json!(hex_digest(&Sha256::digest(content))));
        let out = write(&p).unwrap();
        assert_eq!(out["complete"], json!(true));
        assert_eq!(std::fs::read(&path).unwrap(), content);
        // No partial left behind.
        assert!(!partial_path(&path).exists());

        let mut rp = Map::new();
        rp.insert("path".into(), json!(path.to_string_lossy()));
        let r = read(&rp).unwrap();
        assert_eq!(r["eof"], json!(true));
        assert_eq!(
            b64().decode(r["data_b64"].as_str().unwrap()).unwrap(),
            content
        );
    }

    #[test]
    fn sha256_mismatch_discards_write() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("out.bin");
        let mut p = Map::new();
        p.insert("path".into(), json!(path.to_string_lossy()));
        p.insert("data_b64".into(), json!(b64().encode(b"data")));
        p.insert("final".into(), json!(true));
        p.insert("sha256".into(), json!("00".repeat(32)));
        assert!(write(&p).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn chunked_write_requires_ordered_offsets() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("out.bin");
        let mut p = Map::new();
        p.insert("path".into(), json!(path.to_string_lossy()));
        p.insert("data_b64".into(), json!(b64().encode(b"abc")));
        p.insert("offset".into(), json!(0));
        assert_eq!(write(&p).unwrap()["complete"], json!(false));
        // Wrong next offset.
        let mut p2 = p.clone();
        p2.insert("offset".into(), json!(99));
        assert!(write(&p2).is_err());
        // Correct next offset, final.
        let mut p3 = Map::new();
        p3.insert("path".into(), json!(path.to_string_lossy()));
        p3.insert("data_b64".into(), json!(b64().encode(b"def")));
        p3.insert("offset".into(), json!(3));
        p3.insert("final".into(), json!(true));
        write(&p3).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"abcdef");
    }

    #[test]
    fn no_overwrite_refuses_existing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("out.bin");
        std::fs::write(&path, b"old").unwrap();
        let mut p = Map::new();
        p.insert("path".into(), json!(path.to_string_lossy()));
        p.insert("data_b64".into(), json!(b64().encode(b"new")));
        p.insert("final".into(), json!(true));
        p.insert("overwrite".into(), json!(false));
        assert!(write(&p).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"old");
    }
}
