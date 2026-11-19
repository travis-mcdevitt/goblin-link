//! Runs the real daemon from a copy of its binary and updates or removes that copy the way the
//! Pak Store does. Only through the container runner.
use goblin_ipc::{
    client::{Client, ClientOptions},
    types::*,
};
use goblin_store::{Layout, ProfileStore};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    time::Duration,
};

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Pak {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    data: PathBuf,
    socket: PathBuf,
}

impl Pak {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Goblin Link.pak");
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::copy(env!("CARGO_BIN_EXE_goblind"), dir.join("bin/goblind")).unwrap();
        let data = tmp.path().join("data");
        let store = ProfileStore::open(Layout::new(&data)).unwrap();
        store.create("first").unwrap();
        store.create("second").unwrap();
        store.set_active("first").unwrap();
        let socket = tmp.path().join("run/active");
        Self {
            _tmp: tmp,
            dir,
            data,
            socket,
        }
    }

    fn start(&self) -> Daemon {
        Daemon(
            Command::new(self.dir.join("bin/goblind"))
                .arg("--data-dir")
                .arg(&self.data)
                .arg("--runtime-dir")
                .arg(self.socket.parent().unwrap())
                .env("GOBLIN_PLATFORM", "generic-sdl")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    /// As the store unpacks: the new file is written beside the old and renamed over it.
    fn update(&self) {
        let new = self.dir.join("bin/goblind.tmp");
        std::fs::copy(env!("CARGO_BIN_EXE_goblind"), &new).unwrap();
        std::fs::rename(&new, self.dir.join("bin/goblind")).unwrap();
    }
}

async fn wait_for(socket: &Path, name: &str) -> Client {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(client) = Client::connect(socket, ClientOptions::default()).await {
                if let Ok(status) = client.request::<_>(DaemonStatusParams {}).await {
                    if status.profile == name {
                        return client;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("daemon serves the profile")
}

async fn exit_of(daemon: &mut Daemon, within: Duration) -> Option<ExitStatus> {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if let Some(status) = daemon.0.try_wait().unwrap() {
            return Some(status);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    None
}

#[tokio::test]
async fn an_updated_binary_stops_the_daemon_but_its_own_profile_switch_does_not() {
    let pak = Pak::new();
    let mut daemon = pak.start();
    let client = wait_for(&pak.socket, "first").await;
    // The profile switch runs the binary again as the same process: that is not an update.
    let _: Empty = client
        .request(ProfileSwitchParams {
            name: "second".into(),
        })
        .await
        .unwrap();
    let _client = wait_for(&pak.socket, "second").await;
    assert!(
        exit_of(&mut daemon, Duration::from_secs(7)).await.is_none(),
        "the daemon keeps running after its own restart"
    );
    pak.update();
    let status = exit_of(&mut daemon, Duration::from_secs(15))
        .await
        .expect("the daemon stops once its binary is replaced");
    assert!(status.success(), "a clean stop, not a crash: {status:?}");
}

#[tokio::test]
async fn a_removed_pak_stops_the_daemon() {
    let pak = Pak::new();
    let mut daemon = pak.start();
    let _client = wait_for(&pak.socket, "first").await;
    std::fs::remove_dir_all(&pak.dir).unwrap();
    let status = exit_of(&mut daemon, Duration::from_secs(15))
        .await
        .expect("the daemon stops once its pak is gone");
    assert!(status.success(), "a clean stop, not a crash: {status:?}");
}
