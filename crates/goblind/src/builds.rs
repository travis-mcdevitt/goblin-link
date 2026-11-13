//! What a profile's Muse has built and installed on this device: tools it can call, and
//! apps and games listed in the launcher.
//!
//! Muse builds elsewhere (its own VM), writes the result into a folder inside the profile's
//! own folder with the file commands it already has, and asks for that folder to be
//! installed. A bundle is a folder with a `goblin.json` at its top:
//!
//! ```json
//! {"name": "battery.check", "kind": "tool", "title": "Battery check",
//!  "description": "Reads the battery level",
//!  "run": ["bin/check.sh"], "required": {}, "optional": {}, "timeout_ms": 10000}
//! ```
//!
//! Installing copies the bundle out of Muse's reach, to `<built>/<profile>/installed/<name>`,
//! so what runs later is what was installed (and, when the owner is asked, what was
//! approved). Each profile has its own store: it lists, replaces and removes only its own
//! items, and its tools are offered only to it.
//!
//! Nothing here confines what an item does once it runs. The store decides what may be
//! installed and by whom, not what the installed program can reach.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use goblin_ipc::types::{BuildItem, BuildKind, BuildState};
use serde_json::{json, Map, Value};

use crate::exec::dropins::{self, DropInCommand};

/// The bundle's description, at its top.
pub const MANIFEST: &str = "goblin.json";
/// The store's own record of an item, kept beside its files.
const RECORD: &str = ".goblin-item.json";
/// A bundle may hold this many files.
pub const MAX_FILES: u32 = 2000;
/// A bundle may be this large in all.
pub const MAX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_NAME: usize = 48;
const MAX_TITLE: usize = 40;
const MAX_DESCRIPTION: usize = 2000;

/// Where apps and games are listed. The platform's launcher on a device; a fake in tests.
pub trait Launcher {
    /// Lists `title`, running `command` (an absolute path to one executable).
    fn add(&self, kind: BuildKind, title: &str, command: &Path) -> Result<(), String>;
    /// Removes the entry for `title`.
    fn remove(&self, kind: BuildKind, title: &str) -> Result<(), String>;
}

impl Launcher for goblin_platform::Platform {
    fn add(&self, kind: BuildKind, title: &str, command: &Path) -> Result<(), String> {
        self.register_entry(entry_kind(kind), title, &command.to_string_lossy())
            .map_err(|e| e.to_string())
    }

    fn remove(&self, kind: BuildKind, title: &str) -> Result<(), String> {
        self.unregister_entry(entry_kind(kind), title)
            .map_err(|e| e.to_string())
    }
}

fn entry_kind(kind: BuildKind) -> goblin_platform::EntryKind {
    match kind {
        BuildKind::Game => goblin_platform::EntryKind::Game,
        _ => goblin_platform::EntryKind::Tool,
    }
}

/// One profile's store.
#[derive(Debug, Clone)]
pub struct Builds {
    root: PathBuf,
}

/// A bundle that passed every check, not yet copied anywhere.
struct Bundle {
    item: BuildItem,
    /// The tool's parameter tables, kept for [`Builds::tools`].
    required: Map<String, Value>,
    optional: Map<String, Value>,
    timeout_ms: Option<i64>,
    /// The files, relative to the bundle's top.
    files: Vec<PathBuf>,
}

impl Builds {
    /// The store at `root` (`<built>/<profile>`); nothing is created until it is used.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The store's own folders, writable by the daemon's account alone whatever the umask.
    fn make_store_dirs(&self) -> Result<(), String> {
        if let Some(all) = self.root.parent() {
            make_dir(all)?;
        }
        make_dir(&self.root)?;
        make_dir(&self.dir(BuildState::Pending))?;
        make_dir(&self.dir(BuildState::Installed))
    }

    fn dir(&self, state: BuildState) -> PathBuf {
        self.root.join(match state {
            BuildState::Pending => "pending",
            BuildState::Installed => "installed",
        })
    }

    /// The items, those waiting for the owner first, then by name.
    pub fn list(&self) -> Vec<BuildItem> {
        let mut items = Vec::new();
        for state in [BuildState::Pending, BuildState::Installed] {
            let mut found: Vec<BuildItem> = fs::read_dir(self.dir(state))
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| read_record(&e.path()))
                .filter(|i| i.state == state)
                .collect();
            found.sort_by(|a, b| a.name.cmp(&b.name));
            items.extend(found);
        }
        items
    }

    fn find(&self, state: BuildState, name: &str) -> Option<BuildItem> {
        if !name_ok(name) {
            return None;
        }
        read_record(&self.dir(state).join(name)).filter(|i| i.state == state && i.name == name)
    }

    /// Checks the bundle at `source` and copies it into the store to wait for an answer,
    /// replacing an earlier request of the same name. `now` is Unix seconds.
    pub fn request(&self, source: &Path, now: u64) -> Result<BuildItem, String> {
        self.request_in(source, now, None)
    }

    /// [`Builds::request`], noting the chat it was asked in.
    pub fn request_in(
        &self,
        source: &Path,
        now: u64,
        chat: Option<String>,
    ) -> Result<BuildItem, String> {
        let mut bundle = read_bundle(source)?;
        bundle.item.at = now;
        bundle.item.chat = chat;
        let dest = self.dir(BuildState::Pending).join(&bundle.item.name);
        remove_dir(&dest)?;
        self.make_store_dirs()?;
        if let Err(why) = copy_bundle(source, &dest, &bundle) {
            let _ = fs::remove_dir_all(&dest);
            return Err(why);
        }
        Ok(bundle.item)
    }

    /// Installs the request called `name`, replacing an installed item of that name. An app
    /// or game is listed in `launcher`; `taken` says whether another profile already has an
    /// entry with that kind and title.
    pub fn approve(
        &self,
        name: &str,
        launcher: Option<&dyn Launcher>,
        taken: &dyn Fn(BuildKind, &str) -> bool,
        now: u64,
    ) -> Result<BuildItem, String> {
        let mut item = self
            .find(BuildState::Pending, name)
            .ok_or_else(|| format!("nothing called {name} is waiting to be installed"))?;
        let from = self.dir(BuildState::Pending).join(name);
        let to = self.dir(BuildState::Installed).join(name);
        let before = self.find(BuildState::Installed, name);
        let listed = item.kind != BuildKind::Tool;
        if listed {
            if launcher.is_none() {
                return Err("this device has no launcher to list apps or games in".into());
            }
            if self.list().iter().any(|other| {
                other.state == BuildState::Installed
                    && other.name != name
                    && other.kind == item.kind
                    && other.title.eq_ignore_ascii_case(&item.title)
            }) {
                return Err(format!(
                    "another item already has an entry called {}; choose another title",
                    item.title
                ));
            }
            if taken(item.kind, &item.title) {
                return Err(format!(
                    "another profile already has an entry called {}; choose another title",
                    item.title
                ));
            }
        }
        // The old entry goes first, so a changed title does not leave one behind.
        if let (Some(old), Some(l)) = (&before, launcher) {
            if old.kind != BuildKind::Tool {
                let _ = l.remove(old.kind, &old.title);
            }
        }
        remove_dir(&to)?;
        self.make_store_dirs()?;
        fs::rename(&from, &to).map_err(|e| format!("could not install {name}: {e}"))?;
        item.state = BuildState::Installed;
        item.at = now;
        write_record(&to, &item, None)?;
        if listed {
            let command = to.join(&item.run[0]);
            if let Err(why) = launcher
                .expect("checked above")
                .add(item.kind, &item.title, &command)
            {
                let _ = fs::remove_dir_all(&to);
                return Err(format!("could not list {}: {why}", item.title));
            }
        }
        Ok(item)
    }

    /// Drops the request called `name`.
    pub fn decline(&self, name: &str) -> Result<(), String> {
        self.find(BuildState::Pending, name)
            .ok_or_else(|| format!("nothing called {name} is waiting to be installed"))?;
        remove_dir(&self.dir(BuildState::Pending).join(name))
    }

    /// Removes the item called `name`, installed or waiting, and its launcher entry.
    pub fn remove(&self, name: &str, launcher: Option<&dyn Launcher>) -> Result<(), String> {
        let installed = self.find(BuildState::Installed, name);
        let pending = self.find(BuildState::Pending, name);
        if installed.is_none() && pending.is_none() {
            return Err(format!(
                "this profile has nothing called {name}; only what it installed can be removed"
            ));
        }
        if let (Some(item), Some(l)) = (&installed, launcher) {
            if item.kind != BuildKind::Tool {
                l.remove(item.kind, &item.title)?;
            }
        }
        remove_dir(&self.dir(BuildState::Installed).join(name))?;
        remove_dir(&self.dir(BuildState::Pending).join(name))
    }

    /// Whether this store has an installed entry of that kind and title.
    pub fn has_entry(&self, kind: BuildKind, title: &str) -> bool {
        self.list().iter().any(|i| {
            i.state == BuildState::Installed
                && i.kind == kind
                && i.title.eq_ignore_ascii_case(title)
        })
    }

    /// The installed tools, as commands Muse can call.
    pub fn tools(&self) -> Vec<DropInCommand> {
        let dir = self.dir(BuildState::Installed);
        self.list()
            .into_iter()
            .filter(|i| i.state == BuildState::Installed && i.kind == BuildKind::Tool)
            .filter_map(|item| {
                let home = dir.join(&item.name);
                let record: Value =
                    serde_json::from_slice(&fs::read(home.join(RECORD)).ok()?).ok()?;
                let mut exec = vec![home.join(&item.run[0]).to_string_lossy().into_owned()];
                exec.extend(item.run[1..].iter().cloned());
                let mut def = json!({
                    "description": item.description,
                    "exec": exec,
                    "required": record.get("required").cloned().unwrap_or(json!({})),
                    "optional": record.get("optional").cloned().unwrap_or(json!({})),
                });
                if let Some(t) = record.get("timeout_ms").and_then(Value::as_i64) {
                    def["timeout_ms"] = json!(t);
                }
                match dropins::parse(&item.name, &def) {
                    Ok(cmd) => Some(cmd),
                    Err(why) => {
                        log::warn!("built tool {} is not usable: {why}", item.name);
                        None
                    }
                }
            })
            .collect()
    }
}

/// Whether `name` can name an item: lowercase words, digits, `_` and `-`, joined by dots.
pub fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name.split('.').all(|part| {
            part.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        })
}

fn title_ok(title: &str) -> bool {
    !title.is_empty()
        && title.len() <= MAX_TITLE
        && !title.starts_with('.')
        && title.trim() == title
        && title
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'))
}

/// A path written in a bundle: relative, with nothing but plain names in it.
fn plain_relative(path: &str) -> Option<PathBuf> {
    let p = Path::new(path);
    (!path.is_empty() && p.components().all(|c| matches!(c, Component::Normal(_))))
        .then(|| p.to_path_buf())
}

fn read_bundle(source: &Path) -> Result<Bundle, String> {
    let top = fs::symlink_metadata(source).map_err(|_| "there is no folder at that path")?;
    if !top.is_dir() {
        return Err("that path is not a folder".into());
    }
    let manifest_path = source.join(MANIFEST);
    let meta = fs::symlink_metadata(&manifest_path)
        .map_err(|_| format!("the folder has no {MANIFEST} at its top"))?;
    if !meta.is_file() || meta.len() > MAX_MANIFEST_BYTES {
        return Err(format!("{MANIFEST} must be a file of at most 64 KiB"));
    }
    let manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).map_err(io)?)
        .map_err(|e| format!("{MANIFEST} is not valid JSON: {e}"))?;
    let obj = manifest
        .as_object()
        .ok_or_else(|| format!("{MANIFEST} must be a JSON object"))?;
    const KEYS: [&str; 8] = [
        "name",
        "kind",
        "title",
        "description",
        "run",
        "required",
        "optional",
        "timeout_ms",
    ];
    if let Some(extra) = obj.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!("{MANIFEST} has an unknown key: {extra}"));
    }
    let text = |key: &str| {
        obj.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let name = text("name");
    if !name_ok(&name) {
        return Err(
            "name must be lowercase words (letters, digits, _ or -) joined by dots, at most \
             48 characters"
                .into(),
        );
    }
    let kind = match text("kind").as_str() {
        "tool" => BuildKind::Tool,
        "app" => BuildKind::App,
        "game" => BuildKind::Game,
        _ => return Err("kind must be \"tool\", \"app\" or \"game\"".into()),
    };
    let description = text("description");
    if description.trim().is_empty() || description.len() > MAX_DESCRIPTION {
        return Err("description is required, at most 2000 characters".into());
    }
    let title = match text("title") {
        t if t.is_empty() => name.clone(),
        t => t,
    };
    if kind != BuildKind::Tool && !title_ok(&title) {
        return Err(
            "title is what the launcher shows: letters, digits, spaces, . _ or -, at most 40 \
             characters"
                .into(),
        );
    }
    let run: Vec<String> = match obj.get("run").and_then(Value::as_array) {
        Some(list) if !list.is_empty() && list.iter().all(Value::is_string) => list
            .iter()
            .map(|v| v.as_str().unwrap_or("").to_owned())
            .collect(),
        _ => return Err("run must be a list of strings: the program, then its arguments".into()),
    };
    let program = plain_relative(&run[0])
        .ok_or("run must start with the program's path inside the folder, such as bin/start.sh")?;
    if kind != BuildKind::Tool && run.len() != 1 {
        return Err("an app or game is started without arguments: run must be one path".into());
    }
    let table = |key: &str| -> Result<Map<String, Value>, String> {
        match obj.get(key) {
            None => Ok(Map::new()),
            Some(Value::Object(m)) => Ok(m.clone()),
            Some(_) => Err(format!("{key} must be an object of parameters")),
        }
    };
    let (required, optional) = (table("required")?, table("optional")?);
    let timeout_ms = obj.get("timeout_ms").and_then(Value::as_i64);
    if kind == BuildKind::Tool {
        // The same rules as a command the owner wrote, checked now so Muse hears of a
        // problem when it asks and not when the tool is first called.
        if ["system.", "file.", "device.", "link.", "build."]
            .iter()
            .any(|p| name.starts_with(p))
        {
            return Err(
                "a tool's name must not start with system., file., device., link. or build.".into(),
            );
        }
        if name.split('.').count() < 2 || name.contains('-') {
            return Err(
                "a tool's name is the command Muse calls: at least two words joined by a \
                 dot, without hyphens, such as battery.check"
                    .into(),
            );
        }
        let mut def = json!({
            "description": description,
            "exec": ["/placeholder"],
            "required": required,
            "optional": optional,
        });
        if let Some(t) = timeout_ms {
            def["timeout_ms"] = json!(t);
        }
        dropins::parse(&name, &def).map_err(|e| e.to_string())?;
    } else if !required.is_empty() || !optional.is_empty() || timeout_ms.is_some() {
        return Err("required, optional and timeout_ms are for tools only".into());
    }

    let mut files = Vec::new();
    let mut size = 0u64;
    walk(source, Path::new(""), &mut files, &mut size)?;
    if !files.iter().any(|f| f == &program) {
        return Err(format!("run names {}, which is not in the folder", run[0]));
    }
    Ok(Bundle {
        item: BuildItem {
            name,
            kind,
            state: BuildState::Pending,
            title,
            description,
            run,
            files: files.len() as u32,
            size_bytes: size,
            at: 0,
            chat: None,
        },
        required,
        optional,
        timeout_ms,
        files,
    })
}

/// Lists the regular files under `top/rel`, refusing links and anything that is not a file
/// or a folder.
fn walk(top: &Path, rel: &Path, files: &mut Vec<PathBuf>, size: &mut u64) -> Result<(), String> {
    for entry in fs::read_dir(top.join(rel)).map_err(io)? {
        let entry = entry.map_err(io)?;
        let name = entry.file_name();
        let path = rel.join(&name);
        if name.to_string_lossy() == RECORD {
            return Err(format!("a bundle may not contain a file called {RECORD}"));
        }
        let meta = fs::symlink_metadata(entry.path()).map_err(io)?;
        if meta.is_dir() {
            walk(top, &path, files, size)?;
        } else if meta.is_file() {
            *size += meta.len();
            files.push(path);
            if files.len() as u32 > MAX_FILES {
                return Err(format!("a bundle may hold at most {MAX_FILES} files"));
            }
            if *size > MAX_BYTES {
                return Err(format!(
                    "a bundle may be at most {} MiB",
                    MAX_BYTES / (1024 * 1024)
                ));
            }
        } else {
            return Err(format!(
                "{} is a link or a special file; a bundle holds plain files only",
                path.display()
            ));
        }
    }
    Ok(())
}

fn copy_bundle(source: &Path, dest: &Path, bundle: &Bundle) -> Result<(), String> {
    make_dir(dest)?;
    for rel in &bundle.files {
        let to = dest.join(rel);
        if let Some(parent) = to.parent() {
            make_dir(parent)?;
        }
        // Not through a link, even if one appeared since the walk.
        let from = source.join(rel);
        if !fs::symlink_metadata(&from).map_err(io)?.is_file() {
            return Err(format!("{} changed while it was being read", rel.display()));
        }
        fs::copy(&from, &to).map_err(io)?;
        // Readable and runnable, writable by the daemon's account alone.
        fs::set_permissions(&to, fs::Permissions::from_mode(0o755)).map_err(io)?;
    }
    write_record(dest, &bundle.item, Some(bundle))
}

fn make_dir(dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(io)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).map_err(io)
}

fn write_record(dir: &Path, item: &BuildItem, bundle: Option<&Bundle>) -> Result<(), String> {
    let path = dir.join(RECORD);
    let mut record = match bundle {
        // A new request: the tool's tables come from the manifest.
        Some(b) => {
            let mut r = Map::new();
            r.insert("required".into(), Value::Object(b.required.clone()));
            r.insert("optional".into(), Value::Object(b.optional.clone()));
            if let Some(t) = b.timeout_ms {
                r.insert("timeout_ms".into(), json!(t));
            }
            r
        }
        // A change of state: keep what the record already holds.
        None => fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Map<String, Value>>(&b).ok())
            .unwrap_or_default(),
    };
    if let Ok(Value::Object(fields)) = serde_json::to_value(item) {
        record.extend(fields);
    }
    let text = serde_json::to_vec_pretty(&Value::Object(record)).map_err(|e| e.to_string())?;
    fs::write(&path, text).map_err(io)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).map_err(io)
}

fn read_record(dir: &Path) -> Option<BuildItem> {
    serde_json::from_slice(&fs::read(dir.join(RECORD)).ok()?).ok()
}

fn remove_dir(dir: &Path) -> Result<(), String> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("could not remove {}: {e}", dir.display())),
    }
}

fn io(e: std::io::Error) -> String {
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeLauncher {
        entries: RefCell<Vec<(BuildKind, String, PathBuf)>>,
    }

    impl Launcher for FakeLauncher {
        fn add(&self, kind: BuildKind, title: &str, command: &Path) -> Result<(), String> {
            self.entries
                .borrow_mut()
                .push((kind, title.to_owned(), command.to_owned()));
            Ok(())
        }

        fn remove(&self, kind: BuildKind, title: &str) -> Result<(), String> {
            self.entries
                .borrow_mut()
                .retain(|(k, t, _)| !(*k == kind && t == title));
            Ok(())
        }
    }

    fn bundle(dir: &Path, manifest: Value) -> PathBuf {
        let b = dir.join("bundle");
        let _ = fs::remove_dir_all(&b);
        fs::create_dir_all(b.join("bin")).unwrap();
        fs::write(b.join("bin/run.sh"), "#!/bin/sh\necho '{\"ok\":1}'\n").unwrap();
        fs::write(b.join(MANIFEST), serde_json::to_vec(&manifest).unwrap()).unwrap();
        b
    }

    fn tool() -> Value {
        json!({"name": "battery.check", "kind": "tool", "description": "Reads the battery",
               "run": ["bin/run.sh", "--json"],
               "required": {"unit": {"type": "string", "description": "percent or volts"}}})
    }

    const NOT_TAKEN: &dyn Fn(BuildKind, &str) -> bool = &|_, _| false;

    #[test]
    fn a_tool_waits_then_installs_and_can_be_called() {
        let t = tempfile::tempdir().unwrap();
        let store = Builds::new(t.path().join("built/work"));
        let src = bundle(t.path(), tool());
        let item = store.request(&src, 10).unwrap();
        assert_eq!((item.state, item.files), (BuildState::Pending, 2));
        for dir in ["built", "built/work", "built/work/pending"] {
            let mode = fs::metadata(t.path().join(dir))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755, "{dir}");
        }
        assert!(
            store.tools().is_empty(),
            "nothing is callable while it waits"
        );

        // What was asked for is what gets installed, whatever happens to the source after.
        fs::write(src.join("bin/run.sh"), "#!/bin/sh\nrm -rf /\n").unwrap();
        let item = store.approve("battery.check", None, NOT_TAKEN, 20).unwrap();
        assert_eq!((item.state, item.at), (BuildState::Installed, 20));
        let tools = store.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "battery.check");
        assert_eq!(tools[0].argv[1], "--json");
        assert!(tools[0].required.contains_key("unit"));
        let installed = fs::read_to_string(&tools[0].argv[0]).unwrap();
        assert!(installed.contains("echo"), "the copy taken at the request");
        assert_eq!(store.list().len(), 1);

        store.remove("battery.check", None).unwrap();
        assert!(store.list().is_empty() && store.tools().is_empty());
        assert!(store.remove("battery.check", None).is_err());
    }

    #[test]
    fn a_profile_touches_only_its_own_items() {
        let t = tempfile::tempdir().unwrap();
        let work = Builds::new(t.path().join("built/work"));
        let personal = Builds::new(t.path().join("built/personal"));
        work.request(&bundle(t.path(), tool()), 1).unwrap();
        work.approve("battery.check", None, NOT_TAKEN, 2).unwrap();
        assert!(personal.list().is_empty());
        assert!(personal.tools().is_empty());
        let why = personal.remove("battery.check", None).unwrap_err();
        assert!(why.contains("only what it installed"), "{why}");
        assert_eq!(work.list().len(), 1, "untouched");
    }

    #[test]
    fn an_app_is_listed_and_unlisted() {
        let t = tempfile::tempdir().unwrap();
        let store = Builds::new(t.path().join("built/work"));
        let launcher = FakeLauncher::default();
        let app = json!({"name": "breakout", "kind": "game", "title": "Breakout",
                         "description": "A brick game", "run": ["bin/run.sh"]});
        store.request(&bundle(t.path(), app.clone()), 1).unwrap();
        assert!(store.approve("breakout", None, NOT_TAKEN, 2).is_err());
        store.request(&bundle(t.path(), app.clone()), 1).unwrap();
        // Another profile's entry of that title is not replaced.
        let why = store
            .approve("breakout", Some(&launcher), &|_, _| true, 2)
            .unwrap_err();
        assert!(why.contains("another profile"), "{why}");
        store
            .approve("breakout", Some(&launcher), NOT_TAKEN, 2)
            .unwrap();
        {
            let entries = launcher.entries.borrow();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].1, "Breakout");
            assert!(entries[0].2.ends_with("installed/breakout/bin/run.sh"));
            let mode = fs::metadata(&entries[0].2).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
        // An update under a new title leaves one entry, not two.
        let mut renamed = app;
        renamed["title"] = json!("Breakout 2");
        store.request(&bundle(t.path(), renamed), 3).unwrap();
        store
            .approve("breakout", Some(&launcher), NOT_TAKEN, 4)
            .unwrap();
        assert_eq!(launcher.entries.borrow().len(), 1);
        assert_eq!(launcher.entries.borrow()[0].1, "Breakout 2");
        store.remove("breakout", Some(&launcher)).unwrap();
        assert!(launcher.entries.borrow().is_empty());
    }

    #[test]
    fn launcher_titles_belong_to_one_item_even_with_different_case() {
        for title in ["Breakout", "BREAKOUT"] {
            let t = tempfile::tempdir().unwrap();
            let store = Builds::new(t.path().join("built/work"));
            let launcher = FakeLauncher::default();
            let mut app = json!({"name":"first", "kind":"game", "title":"Breakout",
                "description":"game", "run":["bin/run.sh"]});
            store.request(&bundle(t.path(), app.clone()), 1).unwrap();
            store
                .approve("first", Some(&launcher), NOT_TAKEN, 2)
                .unwrap();
            assert!(store.has_entry(BuildKind::Game, title));
            app["name"] = json!("second");
            app["title"] = json!(title);
            store.request(&bundle(t.path(), app), 3).unwrap();
            assert!(store
                .approve("second", Some(&launcher), NOT_TAKEN, 4)
                .is_err());
            assert_eq!(launcher.entries.borrow().len(), 1);
            assert!(store.find(BuildState::Installed, "first").is_some());
        }
    }

    #[test]
    fn bad_bundles_are_refused_with_a_reason() {
        let t = tempfile::tempdir().unwrap();
        let store = Builds::new(t.path().join("built/work"));
        let refused = |m: Value| store.request(&bundle(t.path(), m), 1).unwrap_err();
        let with = |key: &str, value: Value| {
            let mut m = tool();
            m[key] = value;
            m
        };
        assert!(refused(with("name", json!("../x"))).contains("name must be"));
        assert!(refused(with("name", json!("system.run"))).contains("must not start"));
        assert!(refused(with("name", json!("build.install"))).contains("must not start"));
        assert!(refused(with("kind", json!("driver"))).contains("kind must be"));
        assert!(refused(with("run", json!(["/bin/sh"]))).contains("inside the folder"));
        assert!(refused(with("run", json!(["../../bin/sh"]))).contains("inside the folder"));
        assert!(refused(with("run", json!(["bin/missing"]))).contains("not in the folder"));
        assert!(refused(with("extra", json!(1))).contains("unknown key"));
        assert!(refused(json!({"name": "g", "kind": "game", "title": "a/b",
            "description": "d", "run": ["bin/run.sh"]}))
        .contains("title is"));
        assert!(
            refused(json!({"name": "g", "kind": "app", "description": "d",
            "run": ["bin/run.sh", "--x"]}))
            .contains("without arguments")
        );

        // Links do not come along.
        let b = bundle(t.path(), tool());
        std::os::unix::fs::symlink("/etc/passwd", b.join("bin/secrets")).unwrap();
        assert!(store.request(&b, 1).unwrap_err().contains("link"));
        // Nor does a record of the store's own.
        let b = bundle(t.path(), tool());
        fs::write(b.join(RECORD), "{}").unwrap();
        assert!(store
            .request(&b, 1)
            .unwrap_err()
            .contains("may not contain"));
        assert!(store.list().is_empty(), "nothing refused is kept");
    }

    #[test]
    fn declining_drops_the_request() {
        let t = tempfile::tempdir().unwrap();
        let store = Builds::new(t.path().join("built/work"));
        store.request(&bundle(t.path(), tool()), 1).unwrap();
        store.decline("battery.check").unwrap();
        assert!(store.list().is_empty());
        assert!(store.approve("battery.check", None, NOT_TAKEN, 2).is_err());
        assert!(store.decline("battery.check").is_err());
    }
}
