//! Runs the real daemon's exec/reconnect path, only through the container runner.
use goblin_ipc::{
    client::{Client, ClientOptions},
    types::*,
};
use goblin_store::{profiles::SdkToken, Layout, ProfileStore};
use std::{
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
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
    .expect("daemon serves the selected profile after exec")
}

#[tokio::test]
async fn real_daemon_switches_back_and_forth_with_shared_and_distinct_sdk_tokens() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let runtime = tmp.path().join("run");
    let store = ProfileStore::open(Layout::new(&data)).unwrap();
    let shared = SdkToken::parse(&format!("mgst_{}", "A".repeat(43))).unwrap();
    let first = store.create("first").unwrap();
    let second = store.create("second").unwrap();
    first.save_sdk_token(&shared).unwrap();
    second.save_sdk_token(&shared).unwrap();
    let first_id = first.identity().unwrap().node_id();
    let second_id = second.identity().unwrap().node_id();
    store.set_active("first").unwrap();
    let mut daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_goblind"))
            .arg("--data-dir")
            .arg(&data)
            .arg("--runtime-dir")
            .arg(&runtime)
            // Restart must discard this explicit override and follow the saved choice.
            .args(["--profile", "first"])
            .env("GOBLIN_PLATFORM", "generic-sdl")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let pid = daemon.0.id();
    let socket = runtime.join("active");
    let mut client = wait_for(&socket, "first").await;
    for target in ["second", "first", "second", "first"] {
        let _: Empty = client
            .request(ProfileSwitchParams {
                name: target.into(),
            })
            .await
            .unwrap();
        client = wait_for(&socket, target).await;
        let list: ProfileListResult = client.request(ProfileListParams {}).await.unwrap();
        assert_eq!(list.active, target);
        assert!(list.profiles.iter().all(|p| p.has_sdk_token));
        assert_eq!(daemon.0.id(), pid, "exec keeps the supervised daemon PID");
        assert!(daemon.0.try_wait().unwrap().is_none());
    }
    let different = SdkToken::parse("mgst_abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPA").unwrap();
    second.save_sdk_token(&different).unwrap();
    let _: Empty = client
        .request(ProfileSwitchParams {
            name: "second".into(),
        })
        .await
        .unwrap();
    let _client = wait_for(&socket, "second").await;
    assert_eq!(first.sdk_token().unwrap().unwrap(), shared);
    assert_eq!(second.sdk_token().unwrap().unwrap(), different);
    assert_eq!(first.identity().unwrap().node_id(), first_id);
    assert_eq!(second.identity().unwrap().node_id(), second_id);
    assert_ne!(first_id, second_id);
}
