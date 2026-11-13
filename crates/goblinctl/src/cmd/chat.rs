//! `goblinctl status` and `goblinctl chat …`: talk to the running daemon over its IPC socket.
//!
//! These are thin wrappers over the blocking IPC client, for scripts, for Muse itself through
//! `system.run`, and for testing the daemon without the UI.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::Subcommand;
use goblin_ipc::blocking::Client;
use goblin_ipc::types::{
    ChatCreateParams, ChatHistoryParams, ChatListParams, ChatSendParams, ChatSuggestParams,
    ClientKind, DaemonStatusParams, Role, SubscribeParams, Topic, TurnState,
};
use goblin_ipc::Event;

use crate::error::{CliError, Result};

pub(crate) const TIMEOUT: Duration = Duration::from_secs(10);

/// Where to find the daemon: `--socket`, `$GOBLIN_SOCKET`, else `<runtime>/active`.
fn socket(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(s) = explicit.or_else(|| std::env::var_os("GOBLIN_SOCKET").map(PathBuf::from)) {
        return s;
    }
    let runtime = std::env::var_os("GOBLIND_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("goblin-link"))
        })
        .unwrap_or_else(|| PathBuf::from("/run/goblin-link"));
    goblin_ipc::paths::active_path(&runtime)
}

pub(crate) fn connect(sock: Option<PathBuf>) -> Result<Client> {
    let path = socket(sock);
    Client::connect(&path, ClientKind::Ctl, env!("CARGO_PKG_VERSION"), TIMEOUT)
        .map_err(|e| CliError::Daemon(format!("{} ({e})", path.display())))
}

pub(crate) fn print_json(v: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

/// `goblinctl status`.
pub fn status(sock: Option<PathBuf>, json: bool) -> Result<()> {
    let mut c = connect(sock)?;
    let s = c.request(DaemonStatusParams {}, TIMEOUT)?;
    if json {
        return print_json(&s);
    }
    println!(
        "profile   {}{}",
        s.profile,
        if s.paired { "" } else { " (not paired)" }
    );
    println!("link      {:?}", s.link);
    println!("agent     {}", s.agent.name);
    if let Some(vm) = &s.vm.name {
        println!("vm        {vm}");
    }
    if let Some(b) = &s.battery {
        println!(
            "battery   {}%{}",
            b.percent,
            if b.charging { " charging" } else { "" }
        );
    }
    if let Some(e) = &s.last_error {
        println!("error     {e}");
    }
    println!("version   {} (up {} s)", s.version, s.uptime_s);
    Ok(())
}

/// `goblinctl chat …`
#[derive(Debug, Subcommand)]
pub enum ChatCmd {
    /// List the main chat and the side chats this device knows.
    List,
    /// Show the latest messages of a chat ("main" or a side chat id).
    History {
        /// Chat id.
        #[arg(default_value = "main")]
        chat: String,
        /// How many messages.
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Show the messages before this message id (older ones are read from Muse when the
        /// device has no more).
        #[arg(long)]
        before: Option<String>,
    },
    /// Create a side chat (Muse creates it on its first message).
    Create {
        /// Title shown on this device.
        title: Option<String>,
    },
    /// Set the reply pills: choices for what the owner says back to Muse (Muse calls this
    /// through the `chat.suggest_replies` command). Replies come as arguments, or as JSON `{"replies": [...]}` on stdin.
    Suggest {
        /// The choices (together at most 60 characters). None: read stdin.
        replies: Vec<String>,
        /// Chat id (default: the chat Muse answered last).
        #[arg(long)]
        chat: Option<String>,
        /// Remove the suggestions instead.
        #[arg(long, conflicts_with = "replies")]
        clear: bool,
    },
    /// Send a message. With --wait, wait up to N seconds and print Muse's reply.
    Send {
        /// Message text.
        text: String,
        /// Chat id ("main" or a side chat id).
        #[arg(long, default_value = "main")]
        chat: String,
        /// Seconds to wait for the reply (0: return after Muse acknowledges).
        #[arg(long, default_value_t = 0)]
        wait: u64,
    },
}

/// Runs a `chat` subcommand.
pub fn run(cmd: ChatCmd, sock: Option<PathBuf>, json: bool) -> Result<()> {
    let mut c = connect(sock)?;
    match cmd {
        ChatCmd::List => {
            let r = c.request(ChatListParams {}, TIMEOUT)?;
            if json {
                return print_json(&r);
            }
            for chat in r.chats {
                let busy = if chat.pending_turns > 0 {
                    " (working)"
                } else {
                    ""
                };
                println!("{:<36}  {}{busy}", chat.id, chat.title);
            }
        }
        ChatCmd::History {
            chat,
            limit,
            before,
        } => {
            let r = c.request(
                ChatHistoryParams {
                    chat,
                    before,
                    limit: Some(limit),
                },
                TIMEOUT,
            )?;
            if json {
                return print_json(&r);
            }
            for m in r.messages {
                let who = if m.role == Role::User { "you" } else { "muse" };
                println!("[{who}] {}", m.text);
            }
        }
        ChatCmd::Suggest {
            replies,
            chat,
            clear,
        } => {
            use std::io::IsTerminal;
            let (replies, chat) = if clear {
                (Vec::new(), chat)
            } else if replies.is_empty() {
                if std::io::stdin().is_terminal() {
                    return Err(CliError::Usage(
                        "give the replies as arguments, pipe JSON {\"replies\": [...]}, or use --clear"
                            .into(),
                    ));
                }
                let mut input = String::new();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)
                    .map_err(|e| CliError::Usage(format!("could not read stdin: {e}")))?;
                let v: serde_json::Value = serde_json::from_str(input.trim()).map_err(|_| {
                    CliError::Usage("stdin must be JSON {\"replies\": [...]}".into())
                })?;
                let list = v
                    .get("replies")
                    .and_then(|r| r.as_array())
                    .ok_or_else(|| CliError::Usage("replies must be a list of strings".into()))?
                    .iter()
                    .map(|x| {
                        x.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| CliError::Usage("replies must be strings".into()))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let chat =
                    chat.or_else(|| v.get("chat").and_then(|c| c.as_str()).map(str::to_owned));
                (list, chat)
            } else {
                (replies, chat)
            };
            let n = replies.len();
            let r = c.request(ChatSuggestParams { chat, replies }, TIMEOUT)?;
            if json {
                return print_json(&r);
            }
            if n == 0 {
                println!("Cleared the suggested replies.");
            } else {
                println!("Showing {n} suggested replies on the handheld.");
            }
        }
        ChatCmd::Create { title } => {
            let r = c.request(ChatCreateParams { title }, TIMEOUT)?;
            if json {
                return print_json(&r);
            }
            println!("{}", r.chat.id);
        }
        ChatCmd::Send { text, chat, wait } => {
            if wait > 0 {
                // Subscribe before sending so no reply event can slip past.
                c.subscribe(
                    SubscribeParams {
                        after_seq: None,
                        topics: Some(vec![Topic::Chat]),
                    },
                    TIMEOUT,
                )?;
            }
            let sent = c.request(
                ChatSendParams {
                    chat: chat.clone(),
                    text,
                },
                Duration::from_secs(90),
            )?;
            if wait == 0 {
                return if json {
                    print_json(&sent)
                } else {
                    println!("sent (turn {})", sent.turn);
                    Ok(())
                };
            }
            let reply = wait_reply(&mut c, &chat, &sent.turn, Duration::from_secs(wait))?;
            if json {
                return print_json(&serde_json::json!({
                    "turn": sent.turn, "state": format!("{:?}", reply.0), "reply": reply.1,
                }));
            }
            if reply.1.is_empty() {
                println!("(no reply; turn {:?})", reply.0);
            } else {
                println!("{}", reply.1.join("\n\n"));
            }
            if reply.0 != TurnState::Done {
                return Err(CliError::Daemon(format!("turn ended as {:?}", reply.0)));
            }
        }
    }
    Ok(())
}

/// Collects assistant messages on `chat` until `turn` finishes or `wait` passes.
fn wait_reply(
    c: &mut Client,
    chat: &str,
    turn: &str,
    wait: Duration,
) -> Result<(TurnState, Vec<String>)> {
    let deadline = Instant::now() + wait;
    let mut replies: Vec<(String, String)> = Vec::new();
    let mut state = TurnState::Acked;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Some(frame) = c.next_event(left)? else {
            break;
        };
        match frame.typed()? {
            Event::ChatMessage(m)
                if m.message.chat == chat && m.message.role == Role::Assistant =>
            {
                match replies.iter_mut().find(|(id, _)| *id == m.message.id) {
                    Some(slot) => slot.1 = m.message.text,
                    None => replies.push((m.message.id, m.message.text)),
                }
            }
            Event::ChatTurn(t) if t.turn == turn => {
                state = t.state;
                if matches!(
                    t.state,
                    TurnState::Done | TurnState::Failed | TurnState::Uncertain
                ) {
                    break;
                }
            }
            _ => {}
        }
    }
    Ok((state, replies.into_iter().map(|(_, t)| t).collect()))
}
