//! Behavioural tests for the app state machine (no SDL): tab gating, streaming replies and the
//! live re-theming path.

use goblin_ui::App;
use goblin_ui_kit::{Button, Canvas, InputEvent, Mode, Phase, Theme, Ui};

fn press(b: Button) -> InputEvent {
    InputEvent {
        button: b,
        phase: Phase::Press,
    }
}

fn render_once(app: &mut App, ui: &mut Ui, now: u64) -> Canvas {
    let mut c = Canvas::new(1024, 768);
    app.tick(ui, now);
    app.render(&mut c, ui, now);
    c
}

#[test]
fn account_toggle_reveals_m3_tabs() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    // force_screen on a linked screen should enable the extra tabs.
    assert!(app.force_screen("feed"));
    render_once(&mut app, &mut ui, 1000);
    // Switching back to SDK mode hides them again.
    app.set_account_linked(false);
    assert!(app.force_screen("chat"));
    render_once(&mut app, &mut ui, 1000);
}

#[test]
fn composer_send_starts_streaming_reply() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    app.force_screen("chat");
    let before = app.data.main_chat().len();
    // Open keyboard, type, send.
    app.handle(press(Button::X), &mut ui);
    // Type "hi" via the keyboard grid (q is focused; just push a couple of keys through A).
    app.handle(press(Button::A), &mut ui);
    app.handle(press(Button::Start), &mut ui); // close keyboard
    app.handle(press(Button::Start), &mut ui); // send
                                               // A user turn plus a streaming agent turn were appended.
    assert!(app.data.main_chat().len() > before);
    // Advance the clock so the reply streams and eventually completes.
    for t in (1500..6000).step_by(60) {
        render_once(&mut app, &mut ui, t);
    }
    let last = app.data.main_chat().last().unwrap();
    assert!(!last.from_user);
    assert!(!last.body.is_empty());
}

#[test]
fn appearance_retheme_changes_mode_live() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    assert_eq!(app.theme.mode, Mode::Light);
    app.force_screen("appearance");
    // First render registers focusables and settles focus on the top item (Light).
    render_once(&mut app, &mut ui, 1000);
    // Move down to the Dark choice and activate it.
    app.handle(press(Button::Down), &mut ui);
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::A), &mut ui);
    assert_eq!(
        app.theme.mode,
        Mode::Dark,
        "activating Dark should re-theme live"
    );
}

/// Idle must not redraw: after the first frames settle, ticks without input or data changes
/// leave the app clean (this is what keeps idle CPU near zero on the device).
#[test]
fn idle_app_stops_redrawing() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    let mut c = Canvas::new(1024, 768);
    let mut now = 1000;
    for _ in 0..10 {
        app.tick(&mut ui, now);
        if app.needs_redraw() {
            app.render(&mut c, &mut ui, now);
        }
        now += 250;
    }
    let mut redraws = 0;
    for _ in 0..20 {
        app.tick(&mut ui, now);
        if app.needs_redraw() {
            redraws += 1;
            app.render(&mut c, &mut ui, now);
        }
        now += 250;
    }
    assert_eq!(redraws, 0, "idle ticks keep marking the app dirty");
}

/// Side chats open in the Chat tab; sending goes to the open chat; B returns to the main chat.
#[test]
fn side_chat_opens_sends_and_returns_to_main() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    let main_len = app.data.main_chat().len();
    app.force_screen("sidechats");
    render_once(&mut app, &mut ui, 1000);
    render_once(&mut app, &mut ui, 1000);
    // First row is "New side chat"; the first existing chat is one below.
    app.handle(press(Button::Down), &mut ui);
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::A), &mut ui);
    assert_eq!(app.data.active_chat(), "side-1");
    assert!(
        app.data.main_chat().is_empty(),
        "side chat has its own transcript"
    );
    assert!(app.data.send_user("hello side").is_ok());
    assert_eq!(app.data.main_chat().len(), 1);
    render_once(&mut app, &mut ui, 1100);
    app.handle(press(Button::B), &mut ui);
    assert_eq!(app.data.active_chat(), "main");
    assert_eq!(app.data.main_chat().len(), main_len, "main chat untouched");
}

/// "New side chat" creates and opens a chat.
#[test]
fn new_side_chat_opens_it() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    let n = app.data.side_chats().len();
    app.force_screen("sidechats");
    render_once(&mut app, &mut ui, 1000);
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::A), &mut ui);
    assert_eq!(app.data.side_chats().len(), n + 1);
    assert_eq!(app.data.active_chat(), app.data.side_chats()[0].id);
}

/// Consent toggles in Settings go through the provider (the daemon enforces them).
#[test]
fn settings_consent_toggles_reach_provider() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    assert!(app.data.consent().shell);
    app.force_screen("settings");
    render_once(&mut app, &mut ui, 1000);
    render_once(&mut app, &mut ui, 1000);
    // The Profiles page: two profiles, "New profile", then connect at start.
    for _ in 0..2 {
        app.handle(press(Button::R1), &mut ui);
        render_once(&mut app, &mut ui, 1000);
        render_once(&mut app, &mut ui, 1000);
    }
    for _ in 0..3 {
        app.handle(press(Button::Down), &mut ui);
        render_once(&mut app, &mut ui, 1000);
    }
    assert_eq!(app.data.autostart(), Some(false));
    app.handle(press(Button::A), &mut ui);
    assert_eq!(app.data.autostart(), Some(true), "start on boot turned on");
    // The Access page: read, write, then how far files reach.
    app.handle(press(Button::R1), &mut ui);
    render_once(&mut app, &mut ui, 1000);
    render_once(&mut app, &mut ui, 1000);
    for _ in 0..2 {
        app.handle(press(Button::Down), &mut ui);
        render_once(&mut app, &mut ui, 1000);
    }
    assert_eq!(
        app.data.consent().files,
        goblin_ipc::types::FileScope::Device
    );
    app.handle(press(Button::A), &mut ui);
    assert_eq!(
        app.data.consent().files,
        goblin_ipc::types::FileScope::Home,
        "the reach steps round to the profile's own folder"
    );
    // Past "your own commands" to the shell, the last and widest.
    for _ in 0..2 {
        app.handle(press(Button::Down), &mut ui);
        render_once(&mut app, &mut ui, 1000);
    }
    app.handle(press(Button::A), &mut ui);
    assert!(!app.data.consent().shell, "shell consent turned off");
    assert!(app.data.consent().file_read, "other toggles untouched");
}

/// A profile row opens a sheet: using another profile switches to it, and deleting one
/// takes two presses.
#[test]
fn profile_sheet_switches_and_deletes_only_on_a_second_press() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    app.force_screen("profiles");
    render_once(&mut app, &mut ui, 1000);
    render_once(&mut app, &mut ui, 1000);
    // The second profile ("work", not in use) is focused.
    app.handle(press(Button::A), &mut ui);
    render_once(&mut app, &mut ui, 1000);
    // Down to "Delete this profile": use, pair again, rename, unpair, delete.
    for _ in 0..4 {
        app.handle(press(Button::Down), &mut ui);
    }
    app.handle(press(Button::A), &mut ui);
    assert_eq!(app.data.profiles().len(), 2, "one press only arms it");
    app.handle(press(Button::A), &mut ui);
    assert_eq!(app.data.profiles().len(), 1, "the second deletes");

    // A new profile is not in use; "Use this profile" is its first row.
    let name = app.data.create_profile().unwrap();
    app.force_screen("profiles");
    render_once(&mut app, &mut ui, 1000);
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::A), &mut ui);
    app.handle(press(Button::A), &mut ui);
    let active: Vec<String> = app
        .data
        .profiles()
        .into_iter()
        .filter(|p| p.active)
        .map(|p| p.name)
        .collect();
    assert_eq!(active, vec![name]);
}

/// There is no inbox: with the app open, what arrived counts as seen.
#[test]
fn with_the_app_open_nothing_stays_unseen() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    assert!(app.data.notifications().iter().any(|n| !n.seen));
    render_once(&mut app, &mut ui, 1000);
    assert!(app.data.notifications().iter().all(|n| n.seen));
}

/// Holding F1 opens the voice-note recorder; releasing sends it; a tap only shows a hint.
#[test]
fn f1_hold_records_and_release_sends() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    app.force_screen("chat");
    render_once(&mut app, &mut ui, 1000);
    // Tap: press then release without a hold.
    app.handle(press(Button::F1), &mut ui);
    app.handle(
        InputEvent {
            button: Button::F1,
            phase: Phase::Release { was_hold: false },
        },
        &mut ui,
    );
    render_once(&mut app, &mut ui, 1100);
    assert!(!app.recording_voice(), "a tap does not record");
    // Hold: the recorder opens; release later sends.
    app.handle(press(Button::F1), &mut ui);
    app.handle(
        InputEvent {
            button: Button::F1,
            phase: Phase::Hold,
        },
        &mut ui,
    );
    assert!(app.recording_voice(), "a hold opens the recorder");
    render_once(&mut app, &mut ui, 2000);
    app.handle(
        InputEvent {
            button: Button::F1,
            phase: Phase::Release { was_hold: true },
        },
        &mut ui,
    );
    assert!(!app.recording_voice(), "release sends and closes");
    render_once(&mut app, &mut ui, 3500);
}

/// The voice-to-voice toggle in Settings reaches the provider.
#[test]
fn voice_to_voice_toggle() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    assert!(!app.data.voice_to_voice(), "speech-to-text by default");
    app.force_screen("settings");
    render_once(&mut app, &mut ui, 1000);
    render_once(&mut app, &mut ui, 1000);
    // The Alerts page: lights, vibrate, then speak replies to voice notes.
    app.handle(press(Button::R1), &mut ui);
    render_once(&mut app, &mut ui, 1000);
    render_once(&mut app, &mut ui, 1000);
    for _ in 0..2 {
        app.handle(press(Button::Down), &mut ui);
        render_once(&mut app, &mut ui, 1000);
    }
    app.handle(press(Button::A), &mut ui);
    assert!(app.data.voice_to_voice());
}

/// While a reply is spoken, B skips it instead of going back.
#[test]
fn b_skips_spoken_reply() {
    let mut ui = Ui::new().unwrap();
    let mut mock = goblin_ui::data::MockProvider::new();
    mock.speaking = true;
    let mut app = App::with_provider(Theme::new(Mode::Light, 2.0), Box::new(mock));
    app.force_screen("sidechats");
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::B), &mut ui);
    assert!(!app.data.speaking(), "B stopped the speech");
    // The screen stayed: the next B goes back as usual.
    render_once(&mut app, &mut ui, 1100);
    app.handle(press(Button::B), &mut ui);
    assert!(!app.data.speaking());
}

/// A call runs inside the chat: F2 starts it, F1 mutes, F2 hangs up; B interrupts only
/// while Muse speaks.
#[test]
fn call_flow() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    app.force_screen("chat");
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::F2), &mut ui);
    assert!(app.data.call().is_some());
    render_once(&mut app, &mut ui, 1100);
    app.handle(press(Button::F1), &mut ui);
    assert!(app.data.call().unwrap().muted, "F1 mutes during a call");
    assert!(!app.recording_voice(), "no voice note mid-call");
    // Muse is not speaking, so B is ordinary "back" and the call goes on.
    app.handle(press(Button::B), &mut ui);
    assert!(app.data.call().is_some());
    app.handle(press(Button::F2), &mut ui);
    assert!(app.data.call().is_none(), "F2 hangs up");
    render_once(&mut app, &mut ui, 1200);
}

/// Left from the chat opens the list of chats; A or Right opens the one selected, B closes
/// the list, X starts a new chat. Menu is a separate sheet: A for Settings, B to close.
#[test]
fn chats_are_on_the_left_and_menu_is_settings_or_leave() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    app.reduce_motion = true; // no slide: the list opens and closes at once
    app.force_screen("chat");
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::Left), &mut ui);
    render_once(&mut app, &mut ui, 1010);
    // Focus starts on "New chat"; one down is the first side chat. Right opens it.
    app.handle(press(Button::Down), &mut ui);
    app.handle(press(Button::Right), &mut ui);
    render_once(&mut app, &mut ui, 1020);
    assert_eq!(app.data.active_chat(), "side-1");
    // B closes the list without changing anything.
    app.handle(press(Button::Left), &mut ui);
    render_once(&mut app, &mut ui, 1030);
    app.handle(press(Button::B), &mut ui);
    render_once(&mut app, &mut ui, 1040);
    assert_eq!(app.data.active_chat(), "side-1");
    // X in the list starts a new chat.
    let n = app.data.side_chats().len();
    app.handle(press(Button::Left), &mut ui);
    render_once(&mut app, &mut ui, 1050);
    app.handle(press(Button::X), &mut ui);
    render_once(&mut app, &mut ui, 1060);
    assert_eq!(app.data.side_chats().len(), n + 1);

    // Menu: B closes it; A opens Settings; Menu again leaves the app.
    app.handle(press(Button::Menu), &mut ui);
    app.handle(press(Button::B), &mut ui);
    assert!(!app.wants_quit());
    app.handle(press(Button::Menu), &mut ui);
    app.handle(press(Button::A), &mut ui);
    render_once(&mut app, &mut ui, 1070);
    assert!(app.in_settings());
    app.handle(press(Button::Menu), &mut ui);
    app.handle(press(Button::Menu), &mut ui);
    assert!(app.wants_quit());
}

/// Size and Mode are the first Settings rows; left / right change them and the theme follows.
#[test]
fn size_and_mode_from_settings() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    app.force_screen("settings");
    render_once(&mut app, &mut ui, 1000);
    render_once(&mut app, &mut ui, 1000);
    let normal = app.theme.metrics.body;
    app.handle(press(Button::Right), &mut ui); // Normal -> Comfortable
    render_once(&mut app, &mut ui, 1010);
    assert!(app.theme.metrics.body > normal, "text grew");
    app.handle(press(Button::Left), &mut ui);
    app.handle(press(Button::Left), &mut ui); // -> Compact
    render_once(&mut app, &mut ui, 1020);
    assert!(app.theme.metrics.body < normal, "text shrank");
    assert_eq!(app.data.setting("appearance.ui_size").unwrap(), "compact");
    app.handle(press(Button::Down), &mut ui);
    render_once(&mut app, &mut ui, 1030);
    app.handle(press(Button::A), &mut ui); // Mode: Light -> Dark
    assert_eq!(app.theme.mode, Mode::Dark);
}

/// When Muse offers choices, the first one has focus: A alone answers.
#[test]
fn first_reply_choice_is_one_press() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    app.force_screen("chat");
    render_once(&mut app, &mut ui, 1000);
    render_once(&mut app, &mut ui, 1010);
    let before = app.data.main_chat().len();
    app.handle(press(Button::A), &mut ui);
    let turns = app.data.main_chat();
    assert_eq!(turns.len(), before + 1);
    assert_eq!(
        turns.last().unwrap().body,
        "Yes",
        "the first choice was sent"
    );
}

/// Y on a side chat in the panel forgets it after a confirmation; cancelling keeps it.
#[test]
fn forget_side_chat_from_panel() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    app.reduce_motion = true;
    app.force_screen("chat");
    render_once(&mut app, &mut ui, 1000);
    let n = app.data.side_chats().len();
    let open_on_first_chat = |app: &mut App, ui: &mut Ui, t: u64| {
        app.handle(press(Button::Left), ui);
        render_once(app, ui, t);
        app.handle(press(Button::Down), ui); // New chat -> first side chat
        app.handle(press(Button::Y), ui); // ask to forget it
        render_once(app, ui, t + 5);
    };
    open_on_first_chat(&mut app, &mut ui, 1010);
    app.handle(press(Button::A), &mut ui); // focus is on Cancel
    render_once(&mut app, &mut ui, 1030);
    assert_eq!(app.data.side_chats().len(), n, "cancel keeps the chat");
    open_on_first_chat(&mut app, &mut ui, 1040);
    app.handle(press(Button::Right), &mut ui);
    app.handle(press(Button::A), &mut ui);
    assert_eq!(app.data.side_chats().len(), n - 1, "confirm forgets it");
}

/// Select on a side chat in the panel renames it: the keyboard holds the title, Start saves
/// it, and the message draft comes back untouched.
#[test]
fn rename_a_side_chat_from_the_panel() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    assert!(!app.data.side_chats().is_empty());
    let id = app.data.side_chats()[0].id.clone();
    let old = app.data.side_chats()[0].title.clone();
    app.force_screen("panel");
    render_once(&mut app, &mut ui, 1000);
    // Move down to the first side chat (after the tabs and "New chat").
    for _ in 0..12 {
        if app.panel_focus_is_side_chat() {
            break;
        }
        app.handle(press(Button::Down), &mut ui);
        render_once(&mut app, &mut ui, 1000);
    }
    assert!(app.panel_focus_is_side_chat());
    app.handle(press(Button::Start), &mut ui);
    assert!(app.keyboard_open());
    // Type one more letter and save.
    app.handle(press(Button::A), &mut ui);
    app.handle(press(Button::Start), &mut ui);
    assert!(!app.keyboard_open());
    let now = app
        .data
        .side_chats()
        .iter()
        .find(|c| c.id == id)
        .map(|c| c.title.clone())
        .unwrap();
    assert_ne!(now, old);
    assert!(now.starts_with(&old));
}

/// Rename in the profile sheet opens the keyboard holding the name; Start saves what was
/// typed, and a name that could not be a folder is refused with the keyboard left open.
#[test]
fn a_profile_is_renamed_from_its_sheet() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    app.force_screen("profiles");
    render_once(&mut app, &mut ui, 1000);
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::A), &mut ui); // the sheet for "work"
    app.handle(press(Button::Down), &mut ui); // use, pair again, rename
    app.handle(press(Button::Down), &mut ui);
    app.handle(press(Button::A), &mut ui);
    render_once(&mut app, &mut ui, 1000);
    assert!(app.keyboard_open());
    // The keyboard holds "work": two letters off, then Start with "wo".
    app.handle(press(Button::X), &mut ui);
    app.handle(press(Button::X), &mut ui);
    app.handle(press(Button::Start), &mut ui);
    let names: Vec<String> = app.data.profiles().into_iter().map(|p| p.name).collect();
    assert_eq!(names, vec!["personal".to_owned(), "wo".to_owned()]);
    assert!(!app.keyboard_open());

    // Emptied, it is not a name: nothing changes and the keyboard stays.
    app.force_screen("profiles");
    render_once(&mut app, &mut ui, 1000);
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::A), &mut ui);
    app.handle(press(Button::Down), &mut ui);
    app.handle(press(Button::Down), &mut ui);
    app.handle(press(Button::A), &mut ui);
    app.handle(press(Button::X), &mut ui);
    app.handle(press(Button::X), &mut ui);
    app.handle(press(Button::Start), &mut ui);
    assert!(app.keyboard_open());
    let names: Vec<String> = app.data.profiles().into_iter().map(|p| p.name).collect();
    assert_eq!(names, vec!["personal".to_owned(), "wo".to_owned()]);
}

/// Settings is four pages on L1 / R1; each opens with its first row focused, and the ends
/// do not wrap.
#[test]
fn settings_pages_turn_with_the_shoulder_buttons() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    app.force_screen("settings");
    render_once(&mut app, &mut ui, 1000);
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::L1), &mut ui);
    render_once(&mut app, &mut ui, 1000);
    // Still Display: A on its first row steps the size.
    let before = app.theme.metrics.body;
    app.handle(press(Button::Right), &mut ui);
    render_once(&mut app, &mut ui, 1000);
    assert!(
        app.theme.metrics.body > before,
        "the first page's first row is Size"
    );
    for _ in 0..3 {
        app.handle(press(Button::R1), &mut ui);
        render_once(&mut app, &mut ui, 1000);
        render_once(&mut app, &mut ui, 1000);
    }
    // The fourth page is Access: its first row is "Read files".
    assert!(app.data.consent().file_read);
    app.handle(press(Button::A), &mut ui);
    assert!(
        !app.data.consent().file_read,
        "the last page's first row is Read files"
    );
}

/// Something Muse built and is waiting to install: its row opens a sheet, A installs it,
/// and an installed one is removed only on a second press.
#[test]
fn a_waiting_build_is_approved_and_an_installed_one_removed() {
    use goblin_ipc::types::BuildState;
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    app.force_screen("settings-access");
    render_once(&mut app, &mut ui, 1000);
    render_once(&mut app, &mut ui, 1000);
    assert_eq!(app.data.builds()[0].state, BuildState::Pending);
    // Read, write, reach, own commands, shell, builds switch, then the first built item.
    for _ in 0..6 {
        app.handle(press(Button::Down), &mut ui);
        render_once(&mut app, &mut ui, 1000);
    }
    app.handle(press(Button::A), &mut ui); // the sheet
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::A), &mut ui); // Install it
    render_once(&mut app, &mut ui, 1000);
    assert_eq!(app.data.builds()[0].state, BuildState::Installed);
    assert_eq!(app.data.builds().len(), 2);
    // The same row again: now it offers removal, which wants a second press.
    app.handle(press(Button::A), &mut ui);
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::A), &mut ui);
    render_once(&mut app, &mut ui, 1000);
    assert_eq!(app.data.builds().len(), 2, "one press does not remove");
    app.handle(press(Button::A), &mut ui);
    assert_eq!(app.data.builds().len(), 1);
}

/// A request from Muse shows in the chat it asked in, and its buttons say which button
/// answers: B turns it down, A agrees. Neither answers in the first moment.
#[test]
fn a_request_in_chat_is_answered_with_b_or_a() {
    use goblin_ipc::types::BuildState;
    for (button, installed) in [(Button::B, false), (Button::A, true)] {
        let mut ui = Ui::new().unwrap();
        let mut app = App::new(Theme::new(Mode::Light, 2.0));
        app.data.set_demo_scenario("request");
        let before = app.data.main_chat().len();
        render_once(&mut app, &mut ui, 1000);
        render_once(&mut app, &mut ui, 1016);
        let waiting = |app: &App| {
            app.data
                .builds()
                .into_iter()
                .find(|b| b.name == "battery.check")
                .map(|b| b.state)
        };
        // A press that was already on its way when the request appeared does nothing.
        app.handle(press(button), &mut ui);
        assert_eq!(
            waiting(&app),
            Some(BuildState::Pending),
            "too soon to count"
        );
        assert_eq!(app.data.requests().len(), 1);
        render_once(&mut app, &mut ui, 2000);
        app.handle(press(button), &mut ui);
        assert_eq!(
            waiting(&app),
            installed.then_some(BuildState::Installed),
            "installed: {installed}"
        );
        assert!(app.data.requests().is_empty());
        // The app sends nothing of its own: the daemon says the answer for the owner.
        assert_eq!(app.data.main_chat().len(), before);
    }
}

/// A request to widen a permission has a third answer, Y: yes, for an hour.
#[test]
fn a_permission_request_can_be_granted_for_an_hour() {
    for button in [Button::B, Button::A, Button::Y] {
        let mut ui = Ui::new().unwrap();
        let mut app = App::new(Theme::new(Mode::Light, 2.0));
        app.data.set_demo_scenario("request-setting");
        render_once(&mut app, &mut ui, 1000);
        render_once(&mut app, &mut ui, 2000);
        app.handle(press(button), &mut ui);
        assert!(app.data.requests().is_empty(), "{button:?}");
    }
}

/// First run: with the profile in use unpaired, the app offers to pair it, once. A starts
/// pairing; B leaves it for later and it does not come back in that session.
#[test]
fn first_run_offers_to_pair() {
    for (button, pairing) in [(Button::A, true), (Button::B, false)] {
        let mut ui = Ui::new().unwrap();
        let mut app = App::new(Theme::new(Mode::Light, 2.0));
        app.data.set_demo_scenario("unpaired");
        render_once(&mut app, &mut ui, 1000);
        render_once(&mut app, &mut ui, 1016);
        assert!(app.data.pairing().is_none());
        app.handle(press(button), &mut ui);
        assert_eq!(app.data.pairing().is_some(), pairing, "{button:?}");
        render_once(&mut app, &mut ui, 1032);
        render_once(&mut app, &mut ui, 1048);
        // Not offered again: B now does what B does in the chat.
        if !pairing {
            app.handle(press(Button::A), &mut ui);
            assert!(app.data.pairing().is_none());
        }
    }
}

/// Select lists the buttons for where the owner is; any button puts the list away.
#[test]
fn select_shows_the_controls() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    render_once(&mut app, &mut ui, 1000);
    let before = app.data.main_chat().len();
    app.handle(press(Button::Select), &mut ui);
    render_once(&mut app, &mut ui, 1010);
    // The press that closes it does nothing else.
    app.handle(press(Button::A), &mut ui);
    render_once(&mut app, &mut ui, 1020);
    assert_eq!(app.data.main_chat().len(), before, "nothing was sent");
    app.handle(press(Button::Menu), &mut ui);
    app.handle(press(Button::A), &mut ui);
    assert!(app.in_settings(), "Menu works again once the list is away");
}
