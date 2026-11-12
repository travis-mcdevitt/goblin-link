#![cfg(all(feature = "client", feature = "blocking", feature = "server"))]
//! Client and server against each other over a temporary Unix socket with a fake handler.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use goblin_ipc::blocking;
use goblin_ipc::client::{Client, ClientOptions};
use goblin_ipc::server::{reply, ClientInfo, EventBus, Handler, Server, ServerConfig};
use goblin_ipc::types::*;
use goblin_ipc::{ClientError, ErrorCode, Event, IpcError, Method, ResumeState, Topic};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

struct Fake {
    bus: Arc<EventBus>,
}

impl Handler for Fake {
    async fn handle(
        &self,
        client: ClientInfo,
        method: Method,
    ) -> Result<serde_json::Value, IpcError> {
        match method {
            Method::Hello(_) => reply(&HelloResult {
                daemon_version: "test".into(),
                protocol: 1,
                run_id: self.bus.run_id().to_owned(),
                profile: "dev".into(),
                capabilities: Default::default(),
            }),
            Method::ChatSend(p) => {
                if p.text == "slow" {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
                reply(&ChatSendResult {
                    turn: format!("t-{}", p.text),
                    message_id: None,
                })
            }
            Method::DaemonStatus(_) => Err(IpcError::new(ErrorCode::NotPaired, "not paired")),
            Method::ChatList(_) => {
                assert_ne!(client.kind, ClientKind::Unknown);
                reply(&ChatListResult { chats: vec![] })
            }
            Method::DaemonShutdown(_) => {
                tokio::time::sleep(Duration::from_secs(30)).await;
                reply(&Empty {})
            }
            _ => Err(IpcError::new(ErrorCode::Unavailable, "not in the fake")),
        }
    }
}

struct Running {
    _dir: tempfile::TempDir,
    path: PathBuf,
    bus: Arc<EventBus>,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl Running {
    async fn stop(mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        let _ = self.task.await;
    }
}

fn start_with(bus: Arc<EventBus>, config: ServerConfig) -> Running {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run").join("dev.sock");
    let server = Server::bind(
        &path,
        Arc::new(Fake { bus: bus.clone() }),
        bus.clone(),
        config,
    )
    .unwrap();
    let (tx, rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        server
            .run(async {
                let _ = rx.await;
            })
            .await
            .unwrap();
    });
    Running {
        _dir: dir,
        path,
        bus,
        stop: Some(tx),
        task,
    }
}

fn start() -> Running {
    start_with(Arc::new(EventBus::new("run-1")), ServerConfig::default())
}

fn opts() -> ClientOptions {
    ClientOptions {
        kind: ClientKind::Ui,
        version: "t".into(),
        timeout: Duration::from_secs(5),
    }
}

fn count(n: u32) -> Event {
    Event::NotifyCount(NotifyCountEvent { unseen: n })
}

fn chat_changed() -> Event {
    Event::ChatListChanged(Empty {})
}

async fn raw_exchange(path: &PathBuf, input: &[u8]) -> Vec<u8> {
    let mut s = tokio::net::UnixStream::connect(path).await.unwrap();
    s.write_all(input).await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_millis(400), s.read_to_end(&mut out)).await;
    out
}

#[tokio::test]
async fn hello_typed_request_and_error() {
    let srv = start();
    let c = Client::connect(&srv.path, opts()).await.unwrap();
    assert_eq!(c.hello().run_id, "run-1");
    assert_eq!(c.hello().profile, "dev");
    let r = c.request(ChatListParams {}).await.unwrap();
    assert!(r.chats.is_empty());
    let e = c.request(DaemonStatusParams {}).await.unwrap_err();
    assert!(matches!(e, ClientError::Daemon(ref d) if d.code == ErrorCode::NotPaired));
    let e = c.request(AccountStatusParams {}).await.unwrap_err();
    assert!(matches!(e, ClientError::Daemon(ref d) if d.code == ErrorCode::Unavailable));
    srv.stop().await;
}

#[tokio::test]
async fn pipelined_replies_match_out_of_order() {
    let srv = start();
    let c = Client::connect(&srv.path, opts()).await.unwrap();
    let send = |t: &str| ChatSendParams {
        chat: "main".into(),
        text: t.into(),
        short_replies: false,
    };
    let start = Instant::now();
    let (slow, fast) = tokio::join!(
        async {
            let r = c.request(send("slow")).await.unwrap();
            (r, start.elapsed())
        },
        async {
            let r = c.request(send("fast")).await.unwrap();
            (r, start.elapsed())
        }
    );
    assert_eq!(slow.0.turn, "t-slow");
    assert_eq!(fast.0.turn, "t-fast");
    assert!(fast.1 < slow.1, "fast reply must not wait for the slow one");
    srv.stop().await;
}

#[tokio::test]
async fn request_timeout_leaves_connection_usable() {
    let srv = start();
    let c = Client::connect(&srv.path, opts()).await.unwrap();
    let e = c
        .request_timeout(
            ChatSendParams {
                chat: "main".into(),
                text: "slow".into(),
                short_replies: false,
            },
            Duration::from_millis(50),
        )
        .await
        .unwrap_err();
    assert!(matches!(e, ClientError::Timeout));
    // The late reply is ignored and later requests still work.
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert!(c.request(ChatListParams {}).await.is_ok());
    srv.stop().await;
}

#[tokio::test]
async fn hello_is_required_first_and_only_once() {
    let srv = start();
    let out = raw_exchange(&srv.path, b"{\"id\":1,\"method\":\"chat.list\"}\n").await;
    let v: serde_json::Value =
        serde_json::from_slice(out.split(|&b| b == b'\n').next().unwrap()).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "bad_request");

    let input = b"{\"id\":1,\"method\":\"hello\",\"params\":{\"client\":\"ctl\",\"version\":\"1\"}}\n\
                  {\"id\":2,\"method\":\"hello\",\"params\":{\"client\":\"ctl\",\"version\":\"1\"}}\n\
                  {\"id\":3,\"method\":\"no.such\"}\n\
                  {\"id\":4,\"method\":\"chat.send\",\"params\":{}}\n";
    let out = String::from_utf8(raw_exchange(&srv.path, input).await).unwrap();
    let replies: Vec<serde_json::Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(replies.len(), 4);
    let by_id = |id: u64| replies.iter().find(|r| r["id"] == id).unwrap();
    assert_eq!(by_id(1)["ok"], true);
    assert_eq!(by_id(2)["error"]["code"], "bad_request");
    assert_eq!(by_id(3)["error"]["code"], "unknown_method");
    assert_eq!(by_id(4)["error"]["code"], "bad_request");
    srv.stop().await;
}

#[tokio::test]
async fn overlong_and_garbage_lines_close_the_connection() {
    let srv = start();
    let mut big = vec![b'x'; (1 << 20) + 10];
    big.push(b'\n');
    let out = raw_exchange(&srv.path, &big).await;
    assert!(out.is_empty());
    let out = raw_exchange(&srv.path, b"this is not json\n").await;
    assert!(out.is_empty());
    // The daemon is still fine.
    assert!(Client::connect(&srv.path, opts()).await.is_ok());
    srv.stop().await;
}

#[tokio::test]
async fn subscription_delivers_live_and_replays_after_seq() {
    let srv = start();
    srv.bus.publish(&chat_changed()).unwrap(); // 1
    srv.bus.publish(&count(1)).unwrap(); // 2
    let c = Client::connect(&srv.path, opts()).await.unwrap();
    let r = c
        .subscribe(SubscribeParams {
            after_seq: Some(1),
            topics: None,
        })
        .await
        .unwrap();
    assert_eq!(r, SubscribeResult { seq: 2, gap: false });
    srv.bus.publish(&count(2)).unwrap(); // 3
    let e1 = c.next_event().await.unwrap();
    let e2 = c.next_event().await.unwrap();
    assert_eq!((e1.seq, e2.seq), (2, 3));
    assert_eq!(e1.typed().unwrap(), count(1));
    assert_eq!(e1.event, "notify.count");
    srv.stop().await;
}

#[tokio::test]
async fn topic_filter_and_no_events_before_subscribe() {
    let srv = start();
    let c = Client::connect(&srv.path, opts()).await.unwrap();
    srv.bus.publish(&count(0)).unwrap();
    c.subscribe(SubscribeParams {
        after_seq: None,
        topics: Some(vec![Topic::Chat]),
    })
    .await
    .unwrap();
    srv.bus.publish(&count(1)).unwrap();
    srv.bus.publish(&chat_changed()).unwrap();
    let e = tokio::time::timeout(Duration::from_secs(2), c.next_event())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(e.event, "chat.list_changed");
    srv.stop().await;
}

#[tokio::test]
async fn gap_is_reported_when_the_ring_is_exceeded() {
    let bus = Arc::new(EventBus::with_capacity("run-1", 4));
    let srv = start_with(bus, ServerConfig::default());
    for i in 0..10 {
        srv.bus.publish(&count(i)).unwrap();
    }
    let c = Client::connect(&srv.path, opts()).await.unwrap();
    let r = c
        .subscribe(SubscribeParams {
            after_seq: Some(2),
            topics: None,
        })
        .await
        .unwrap();
    assert!(r.gap);
    assert_eq!(r.seq, 10);
    srv.stop().await;
}

#[tokio::test]
async fn resume_after_reconnect_and_after_daemon_restart() {
    let srv = start();
    let mut resume = ResumeState::new();
    {
        let c = Client::connect(&srv.path, opts()).await.unwrap();
        let r = c.subscribe_resuming(&resume, None).await.unwrap();
        assert_eq!(r.seq, 0);
        srv.bus.publish(&count(1)).unwrap();
        let e = c.next_event().await.unwrap();
        resume.observe(&c.hello().run_id, e.seq);
    }
    srv.bus.publish(&count(2)).unwrap(); // missed while disconnected
    let c = Client::connect(&srv.path, opts()).await.unwrap();
    let r = c.subscribe_resuming(&resume, None).await.unwrap();
    assert!(!r.gap);
    assert_eq!(c.next_event().await.unwrap().seq, 2);

    // A restarted daemon has a new run id: no after_seq is sent.
    let srv2 = start_with(Arc::new(EventBus::new("run-2")), ServerConfig::default());
    let c2 = Client::connect(&srv2.path, opts()).await.unwrap();
    assert_eq!(resume.after_seq_for(&c2.hello().run_id), None);
    srv.stop().await;
    srv2.stop().await;
}

#[tokio::test]
async fn slow_client_is_disconnected_and_others_unaffected() {
    let config = ServerConfig {
        queue_capacity: 4,
        write_timeout: Duration::from_millis(300),
        ..ServerConfig::default()
    };
    let srv = start_with(Arc::new(EventBus::new("run-1")), config);
    // A client that subscribes and then never reads.
    let mut slow = tokio::net::UnixStream::connect(&srv.path).await.unwrap();
    slow.write_all(
        b"{\"id\":1,\"method\":\"hello\",\"params\":{\"client\":\"ui\",\"version\":\"1\"}}\n\
          {\"id\":2,\"method\":\"subscribe\",\"params\":{}}\n",
    )
    .await
    .unwrap();
    let good = Client::connect(&srv.path, opts()).await.unwrap();
    good.subscribe(SubscribeParams::default()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let payload = "x".repeat(60_000);
    let big = Event::Unknown {
        event: "x.big".into(),
        data: serde_json::json!({"p": payload}),
    };
    let started = Instant::now();
    for _ in 0..200 {
        srv.bus.publish(&big).unwrap();
        // drain the good client so only the idle one falls behind
        while let Ok(Some(_)) =
            tokio::time::timeout(Duration::from_millis(1), good.next_event()).await
        {}
    }
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "publishing must not block"
    );
    assert_eq!(
        srv.bus.subscriber_count(),
        1,
        "only the healthy client remains"
    );

    // The slow connection is closed: reading drains what was buffered, then hits EOF.
    let mut sink = Vec::new();
    let eof = tokio::time::timeout(Duration::from_secs(5), slow.read_to_end(&mut sink)).await;
    assert!(eof.is_ok(), "slow client must be disconnected");
    assert!(good.request(ChatListParams {}).await.is_ok());
    srv.stop().await;
}

#[tokio::test]
async fn socket_permissions_stale_socket_and_cleanup() {
    let srv = start();
    let mode = std::fs::metadata(&srv.path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let dir_mode = std::fs::metadata(srv.path.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700);

    // A second daemon must not steal a live socket.
    let bus = Arc::new(EventBus::new("x"));
    let again = Server::bind(
        &srv.path,
        Arc::new(Fake { bus: bus.clone() }),
        bus,
        ServerConfig::default(),
    );
    assert_eq!(
        again.err().map(|e| e.kind()),
        Some(std::io::ErrorKind::AddrInUse)
    );

    let path = srv.path.clone();
    srv.stop().await;
    assert!(!path.exists(), "socket removed on shutdown");
}

#[tokio::test]
async fn stale_socket_is_replaced_and_regular_files_are_protected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dev.sock");
    {
        // Leave a stale socket file behind.
        let l = std::os::unix::net::UnixListener::bind(&path).unwrap();
        drop(l);
    }
    assert!(path.exists());
    let bus = Arc::new(EventBus::new("x"));
    let s = Server::bind(
        &path,
        Arc::new(Fake { bus: bus.clone() }),
        bus.clone(),
        ServerConfig::default(),
    );
    assert!(s.is_ok());
    drop(s);
    assert!(!path.exists());

    let file = dir.path().join("plain");
    std::fs::write(&file, "data").unwrap();
    let r = Server::bind(
        &file,
        Arc::new(Fake { bus: bus.clone() }),
        bus,
        ServerConfig::default(),
    );
    assert!(r.is_err());
    assert!(file.exists());
}

#[tokio::test]
async fn client_sees_closed_connection_when_daemon_stops() {
    let srv = start();
    let c = Client::connect(&srv.path, opts()).await.unwrap();
    let pending = c.request(DaemonShutdownParams {});
    let stopper = srv.stop();
    let (r, ()) = tokio::join!(pending, stopper);
    assert!(matches!(r, Err(ClientError::Closed)));
    assert!(c.next_event().await.is_none());
    assert!(matches!(
        c.request(ChatListParams {}).await,
        Err(ClientError::Closed)
    ));
}

#[test]
fn blocking_client_request_subscribe_and_poll() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dev.sock");
    let bus = Arc::new(EventBus::new("run-b"));
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let thread_bus = bus.clone();
    let thread_path = path.clone();
    let server = std::thread::spawn(move || {
        rt.block_on(async move {
            let s = Server::bind(
                &thread_path,
                Arc::new(Fake {
                    bus: thread_bus.clone(),
                }),
                thread_bus,
                ServerConfig::default(),
            )
            .unwrap();
            ready_tx.send(()).unwrap();
            s.run(async {
                let _ = stop_rx.await;
            })
            .await
            .unwrap();
        });
    });
    ready_rx.recv().unwrap();

    let t = Duration::from_secs(5);
    let mut c = blocking::Client::connect(&path, ClientKind::Ctl, "t", t).unwrap();
    assert_eq!(c.hello().run_id, "run-b");
    let r = c.subscribe(SubscribeParams::default(), t).unwrap();
    assert_eq!(r.seq, 0);

    // A reply that arrives together with events is matched by id; events stay queued.
    bus.publish(&count(1)).unwrap();
    bus.publish(&count(2)).unwrap();
    let list = c.request(ChatListParams {}, t).unwrap();
    assert!(list.chats.is_empty());
    let e = c.next_event(t).unwrap().unwrap();
    assert_eq!(e.seq, 1);
    let e = c.next_event(t).unwrap().unwrap();
    assert_eq!(e.seq, 2);
    assert!(c.next_event(Duration::from_millis(50)).unwrap().is_none());

    let err = c.request(DaemonStatusParams {}, t).unwrap_err();
    assert!(matches!(err, ClientError::Daemon(d) if d.code == ErrorCode::NotPaired));

    // Polling style, as a UI frame loop would use it.
    c.set_nonblocking(true).unwrap();
    assert!(c.try_recv().unwrap().is_none());
    let id = c
        .send(ChatSendParams {
            chat: "main".into(),
            text: "poll".into(),
            short_replies: false,
        })
        .unwrap();
    bus.publish(&count(3)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let (mut got_reply, mut got_event) = (false, false);
    while !(got_reply && got_event) {
        assert!(Instant::now() < deadline, "timed out polling");
        match c.try_recv().unwrap() {
            Some(blocking::Incoming::Response(r)) => {
                assert_eq!(r.id, id);
                let v: ChatSendResult = serde_json::from_value(r.into_value().unwrap()).unwrap();
                assert_eq!(v.turn, "t-poll");
                got_reply = true;
            }
            Some(blocking::Incoming::Event(e)) => {
                assert_eq!(e.seq, 3);
                got_event = true;
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }

    stop_tx.send(()).unwrap();
    server.join().unwrap();
    assert!(matches!(
        loop {
            match c.try_recv() {
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                other => break other,
            }
        },
        Err(ClientError::Closed)
    ));
}
