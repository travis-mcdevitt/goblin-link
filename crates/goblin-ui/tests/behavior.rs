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
fn composer_send_starts_streaming_reply() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    assert!(app.force_screen("chat"));
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

/// A side chat opens from the chat list; sending goes to the open chat; B returns to the
/// main chat.
#[test]
fn side_chat_opens_sends_and_returns_to_main() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    app.reduce_motion = true; // the list opens and closes at once
    let main_len = app.data.main_chat().len();
    assert!(app.force_screen("chat"));
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::Left), &mut ui);
    render_once(&mut app, &mut ui, 1000);
    // Main chat, "New chat", then the first side chat.
    app.handle(press(Button::Down), &mut ui);
    app.handle(press(Button::Down), &mut ui);
    app.handle(press(Button::A), &mut ui);
    render_once(&mut app, &mut ui, 1000);
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

/// "New chat" in the chat list creates a side chat and opens it.
#[test]
fn new_side_chat_opens_it() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    app.reduce_motion = true;
    let n = app.data.side_chats().len();
    assert!(app.force_screen("chat"));
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::Left), &mut ui);
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::Down), &mut ui);
    app.handle(press(Button::A), &mut ui);
    render_once(&mut app, &mut ui, 1000);
    assert_eq!(app.data.side_chats().len(), n + 1);
    assert_eq!(app.data.active_chat(), app.data.side_chats()[0].id);
}

/// Consent toggles in Settings go through the provider (the daemon enforces them).
#[test]
fn settings_consent_toggles_reach_provider() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    assert!(app.data.consent().shell);
    assert!(app.force_screen("settings"));
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
    // Past outside writes and "your own commands" to the shell.
    for _ in 0..3 {
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
    assert!(app.force_screen("profiles"));
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

    // A new profile has no agent: pairing is its first row, switching to it the second.
    let name = app.data.create_profile().unwrap();
    assert!(app.force_screen("profiles"));
    render_once(&mut app, &mut ui, 1000);
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::A), &mut ui);
    app.handle(press(Button::Down), &mut ui);
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
    assert!(app.data.unseen_notices());
    render_once(&mut app, &mut ui, 1000);
    assert!(!app.data.unseen_notices());
}

/// Holding F1 opens the voice-note recorder; releasing sends it; a tap only shows a hint.
#[test]
fn f1_hold_records_and_release_sends() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    assert!(app.force_screen("chat"));
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
    assert!(app.force_screen("settings"));
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
    assert!(app.force_screen("settings"));
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::B), &mut ui);
    assert!(!app.data.speaking(), "B stopped the speech");
    assert!(app.in_settings(), "and did nothing else");
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
    assert!(app.force_screen("chat"));
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
    assert!(app.force_screen("chat"));
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::Left), &mut ui);
    render_once(&mut app, &mut ui, 1010);
    // The chat the owner is in is the one selected, so Right alone goes back to it.
    app.handle(press(Button::Right), &mut ui);
    render_once(&mut app, &mut ui, 1012);
    assert_eq!(app.data.active_chat(), "main");
    // Down past "New chat" to the first side chat; Right opens it.
    app.handle(press(Button::Left), &mut ui);
    render_once(&mut app, &mut ui, 1014);
    app.handle(press(Button::Down), &mut ui);
    app.handle(press(Button::Down), &mut ui);
    app.handle(press(Button::Right), &mut ui);
    render_once(&mut app, &mut ui, 1020);
    assert_eq!(app.data.active_chat(), "side-1");
    // Opened again from there, it is that chat that is selected.
    app.handle(press(Button::Left), &mut ui);
    render_once(&mut app, &mut ui, 1022);
    assert!(app.panel_focus_is_side_chat());
    app.handle(press(Button::Right), &mut ui);
    render_once(&mut app, &mut ui, 1024);
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

    // Menu opens Settings, and B or Menu closes it; the first row there leaves the app.
    app.handle(press(Button::Menu), &mut ui);
    render_once(&mut app, &mut ui, 1070);
    assert!(app.in_settings());
    app.handle(press(Button::B), &mut ui);
    assert!(!app.in_settings() && !app.wants_quit());
    app.handle(press(Button::Menu), &mut ui);
    app.handle(press(Button::Menu), &mut ui);
    assert!(!app.in_settings() && !app.wants_quit());
    app.handle(press(Button::Menu), &mut ui);
    render_once(&mut app, &mut ui, 1080);
    app.handle(press(Button::A), &mut ui);
    assert!(app.wants_quit());
}

/// Size and Mode are the first Settings rows; left / right change them and the theme follows.
#[test]
fn size_and_mode_from_settings() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    assert!(app.force_screen("settings"));
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
    assert!(app.force_screen("chat"));
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

/// A changing list must not change the chat selected for a delayed action.
#[test]
fn panel_actions_keep_the_pressed_chat_when_the_list_changes() {
    for forget in [true, false] {
        let mut ui = Ui::new().unwrap();
        let mut app = App::new(Theme::new(Mode::Light, 2.0));
        app.reduce_motion = true;
        assert!(app.force_screen("chat"));
        render_once(&mut app, &mut ui, 1000);
        let ids: Vec<_> = app.data.side_chats().iter().map(|c| c.id.clone()).collect();
        assert!(ids.len() >= 3);
        app.handle(press(Button::Left), &mut ui);
        render_once(&mut app, &mut ui, 1010);
        for _ in 0..3 {
            app.handle(press(Button::Down), &mut ui);
        }
        app.handle(press(if forget { Button::Y } else { Button::A }), &mut ui);
        app.data.forget_side_chat(&ids[0]).unwrap();
        if forget {
            app.handle(press(Button::Right), &mut ui);
            app.handle(press(Button::A), &mut ui);
            assert!(!app.data.side_chats().iter().any(|c| c.id == ids[1]));
            assert!(app.data.side_chats().iter().any(|c| c.id == ids[2]));
        } else {
            render_once(&mut app, &mut ui, 1020);
            assert_eq!(app.data.active_chat(), ids[1]);
        }
    }
}

/// Y on a side chat in the panel forgets it after a confirmation; cancelling keeps it.
#[test]
fn forget_side_chat_from_panel() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    app.reduce_motion = true;
    assert!(app.force_screen("chat"));
    render_once(&mut app, &mut ui, 1000);
    let n = app.data.side_chats().len();
    let open_on_first_chat = |app: &mut App, ui: &mut Ui, t: u64| {
        app.handle(press(Button::Left), ui);
        render_once(app, ui, t);
        app.handle(press(Button::Down), ui); // Main chat -> New chat
        app.handle(press(Button::Down), ui); // -> first side chat
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
    assert!(app.force_screen("panel"));
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
    assert!(app.force_screen("profiles"));
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
    assert!(app.force_screen("profiles"));
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

/// Settings is pages on L1 / R1; each opens with its first row focused, and the ends do
/// not wrap.
#[test]
fn settings_pages_turn_with_the_shoulder_buttons() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    assert!(app.force_screen("settings"));
    render_once(&mut app, &mut ui, 1000);
    render_once(&mut app, &mut ui, 1000);
    // Display: its first row steps the size.
    let before = app.theme.metrics.body;
    app.handle(press(Button::Right), &mut ui);
    render_once(&mut app, &mut ui, 1000);
    assert!(
        app.theme.metrics.body > before,
        "Display's first row is Size"
    );
    // Back past the first page (the ways out): it stays, and forward again is Display.
    for button in [Button::L1, Button::L1, Button::R1] {
        app.handle(press(button), &mut ui);
        render_once(&mut app, &mut ui, 1000);
        render_once(&mut app, &mut ui, 1000);
    }
    let before = app.theme.metrics.body;
    app.handle(press(Button::Right), &mut ui);
    render_once(&mut app, &mut ui, 1000);
    assert_ne!(app.theme.metrics.body, before, "Display again: Size");
    // One more press than there are pages after Display: the last page stays.
    for _ in 0..4 {
        app.handle(press(Button::R1), &mut ui);
        render_once(&mut app, &mut ui, 1000);
        render_once(&mut app, &mut ui, 1000);
    }
    // The last page is Access: its first row is "Read files".
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
    assert!(app.force_screen("settings-access"));
    render_once(&mut app, &mut ui, 1000);
    render_once(&mut app, &mut ui, 1000);
    assert_eq!(app.data.builds()[0].state, BuildState::Pending);
    // Read, write, reach, outside writes, own commands, shell, builds, then the first item.
    for _ in 0..7 {
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

/// The two screens the handhelds have, at every text size.
fn every_screen() -> Vec<((u32, u32), goblin_ui_kit::UiSize)> {
    [(1024, 768), (640, 480)]
        .into_iter()
        .flat_map(|screen| goblin_ui_kit::UiSize::ALL.map(|size| (screen, size)))
        .collect()
}

/// The chat with `scenario`'s request waiting, drawn on that screen at that text size and
/// left there long enough for a press to count.
fn chat_with_request(
    scenario: &str,
    (w, h): (u32, u32),
    size: goblin_ui_kit::UiSize,
) -> (App, Ui, Canvas) {
    let mut ui = Ui::new().unwrap();
    let mut theme = Theme::new(Mode::Dark, goblin_ui_kit::scale_for(w, h));
    theme.set_ui_size(size);
    let mut app = App::new(theme);
    app.data.set_demo_scenario(scenario);
    let mut canvas = Canvas::new(w, h);
    for now in [1000, 1016, 2000] {
        app.tick(&mut ui, now);
        app.render(&mut canvas, &mut ui, now);
    }
    // Whatever is asked, its card stays between the header and the answers: it never
    // reaches past the screen, and never under the agent's name.
    let card = app.request_card().expect("a card");
    let at = format!("{scenario} on {w}x{h} at {size:?}: {card:?}");
    assert!(card.ceiling > 0.0, "{at}");
    assert!(card.rect.y >= card.ceiling, "{at}");
    assert!(card.rect.bottom() < h as f32, "{at}");
    assert!(card.rect.x >= 0.0 && card.rect.right() <= w as f32, "{at}");
    (app, ui, canvas)
}

/// The owner gives leave only for what the card showed whole. A Hermes's approval with
/// more lines than the card holds, one the handheld had to cut when it arrived, one with
/// a character in it that no screen shows, and an install described at more length than
/// fits: each says it is not whole and has one answer, no, on B. A and Y answer nothing,
/// on either screen at any text size. (The requests here list all their answers, as the
/// daemon would not for the cut one: it is the app that leaves only the no.)
#[test]
fn a_request_the_card_cannot_show_whole_can_only_be_refused() {
    for (screen, size) in every_screen() {
        for (scenario, id) in [
            ("request-long", "prompt:demo"),
            ("request-cut", "prompt:demo"),
            ("request-sly", "prompt:demo"),
            ("request-wordy", "install:battery.check:demo"),
        ] {
            let at = format!("{scenario} on {screen:?} at {size:?}");
            let (mut app, mut ui, mut canvas) = chat_with_request(scenario, screen, size);
            let card = app.request_card().expect("a card");
            assert!(!card.whole, "{at}");
            for button in [Button::A, Button::Y] {
                app.handle(press(button), &mut ui);
                app.tick(&mut ui, 2016);
                app.render(&mut canvas, &mut ui, 2016);
                assert!(app.data.demo_answers().is_empty(), "{button:?}: {at}");
                assert_eq!(app.data.requests().len(), 1, "{button:?}: {at}");
            }
            app.handle(press(Button::B), &mut ui);
            assert_eq!(
                app.data.demo_answers(),
                [(id.to_owned(), "no".to_owned())],
                "{at}"
            );
        }
    }
}

/// An approval that the card shows whole keeps all its answers, each on its button. A
/// short command fits on either screen at every text size.
#[test]
fn an_approval_shown_whole_is_answered_with_its_buttons() {
    for (screen, size) in every_screen() {
        for (button, answer) in [(Button::A, "yes"), (Button::Y, "hour"), (Button::B, "no")] {
            let (mut app, mut ui, _) = chat_with_request("request-approval", screen, size);
            let card = app.request_card().expect("a card");
            assert!(card.whole, "{screen:?} at {size:?}");
            app.handle(press(button), &mut ui);
            assert_eq!(
                app.data.demo_answers(),
                [("prompt:demo".to_owned(), answer.to_owned())],
                "{screen:?} at {size:?}"
            );
        }
    }
}

/// The cards the handheld writes itself, an install and a change to a setting, are whole
/// on either screen at every text size, and stay within the screen like any other (which
/// [`chat_with_request`] checks of every card it draws). So is a change to a setting with
/// the longest reason Muse may give for one, in the widest letters.
#[test]
fn the_handhelds_own_cards_are_whole_on_every_screen() {
    for (screen, size) in every_screen() {
        for (scenario, answers) in [
            ("request", 2),
            ("request-setting", 3),
            ("request-setting-long", 3),
        ] {
            let (app, ..) = chat_with_request(scenario, screen, size);
            let card = app.request_card().expect("a card");
            assert!(card.whole, "{scenario} on {screen:?} at {size:?}");
            assert_eq!(
                card.answers, answers,
                "{scenario} on {screen:?} at {size:?}"
            );
        }
    }
}

/// A question from a Hermes gives leave for nothing. One too long for its card says so
/// and keeps its answers.
#[test]
fn a_question_too_long_for_its_card_stays_answerable() {
    for (screen, size) in every_screen() {
        let (mut app, mut ui, _) = chat_with_request("request-question", screen, size);
        let card = app.request_card().expect("a card");
        assert!(!card.whole, "{screen:?} at {size:?}");
        assert_eq!(card.answers, 3, "{screen:?} at {size:?}");
        app.handle(press(Button::Y), &mut ui);
        assert_eq!(
            app.data.demo_answers(),
            [("prompt:demo".to_owned(), "hour".to_owned())],
            "{screen:?} at {size:?}"
        );
    }
}

/// An answer is given only by a button the screen shows. Where the row has no room for an
/// answer's button, pressing that button answers nothing.
#[test]
fn an_answer_without_its_button_on_screen_is_not_given() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Dark, 2.0));
    app.data.set_demo_scenario("request-question");
    // Too narrow for more than the first of its three answers.
    let mut canvas = Canvas::new(360, 768);
    for now in [1000, 1016, 2000] {
        app.tick(&mut ui, now);
        app.render(&mut canvas, &mut ui, now);
    }
    assert_eq!(app.request_card().expect("a card").answers, 1);
    for button in [Button::Y, Button::B] {
        app.handle(press(button), &mut ui);
        app.tick(&mut ui, 2016);
        app.render(&mut canvas, &mut ui, 2016);
        assert!(app.data.demo_answers().is_empty(), "{button:?}");
    }
    app.handle(press(Button::A), &mut ui);
    assert_eq!(
        app.data.demo_answers(),
        [("prompt:demo".to_owned(), "yes".to_owned())]
    );
}

/// A card answers for the words it shows. When the same request comes again with other
/// words, a press already on its way answers nothing: not before the new words are drawn,
/// and not in their first moment on screen.
#[test]
fn a_request_whose_words_change_is_not_answered_by_a_press_on_its_way() {
    let (mut app, mut ui, mut canvas) =
        chat_with_request("request-approval", (1024, 768), Default::default());
    app.data.set_demo_scenario("request-other");
    app.handle(press(Button::A), &mut ui);
    assert!(app.data.demo_answers().is_empty(), "not drawn yet");
    app.tick(&mut ui, 2016);
    app.render(&mut canvas, &mut ui, 2016);
    app.handle(press(Button::A), &mut ui);
    assert!(app.data.demo_answers().is_empty(), "too soon to count");
    app.tick(&mut ui, 3000);
    app.render(&mut canvas, &mut ui, 3000);
    app.handle(press(Button::A), &mut ui);
    assert_eq!(
        app.data.demo_answers(),
        [("prompt:demo".to_owned(), "yes".to_owned())]
    );
}

/// The first moment, in which presses do not count, belongs to the answers on the screen.
/// It starts again when the card comes back from under the keyboard, an overlay or
/// another screen: a press then is no more meant for it than one made as it first
/// appeared.
#[test]
fn a_card_that_was_covered_has_its_first_moment_again() {
    // X opens the keyboard, Select the controls and Menu the settings; B closes each.
    for cover in [Button::X, Button::Select, Button::Menu] {
        let (mut app, mut ui, mut canvas) =
            chat_with_request("request-approval", (1024, 768), Default::default());
        let mut frame = |app: &mut App, ui: &mut Ui, now: u64| {
            app.tick(ui, now);
            app.render(&mut canvas, ui, now);
        };
        app.handle(press(cover), &mut ui);
        frame(&mut app, &mut ui, 2016);
        assert!(app.request_card().is_none(), "{cover:?} covers the card");
        // Covered for longer than the moment lasts.
        frame(&mut app, &mut ui, 4000);
        app.handle(press(Button::B), &mut ui);
        // Not drawn again yet, then drawn this instant: A answers nothing either time.
        app.handle(press(Button::A), &mut ui);
        frame(&mut app, &mut ui, 4016);
        assert!(app.request_card().is_some(), "{cover:?}: the card is back");
        app.handle(press(Button::A), &mut ui);
        assert!(app.data.demo_answers().is_empty(), "{cover:?}: too soon");
        assert_eq!(app.data.requests().len(), 1, "{cover:?}");
        frame(&mut app, &mut ui, 5000);
        app.handle(press(Button::A), &mut ui);
        assert_eq!(
            app.data.demo_answers(),
            [("prompt:demo".to_owned(), "yes".to_owned())],
            "{cover:?}"
        );
    }
}

/// A card that could only be refused becomes whole when the text is made smaller, and its
/// yes appears. That yes has a first moment of its own: A, pressed as it appears, answers
/// nothing, however long the card had been there with its no.
#[test]
fn answers_that_appear_on_a_card_have_a_first_moment_of_their_own() {
    let (mut app, mut ui, mut canvas) =
        chat_with_request("request-medium", (640, 480), goblin_ui_kit::UiSize::Large);
    assert!(!app.request_card().expect("a card").whole);
    app.tick(&mut ui, 5000);
    app.render(&mut canvas, &mut ui, 5000);
    app.data
        .set_setting("appearance.ui_size", serde_json::json!("compact"))
        .unwrap();
    // Before the smaller text is drawn the card is still the one that was cut.
    app.handle(press(Button::A), &mut ui);
    app.tick(&mut ui, 5016);
    app.render(&mut canvas, &mut ui, 5016);
    assert!(app.request_card().expect("a card").whole);
    app.handle(press(Button::A), &mut ui);
    assert!(app.data.demo_answers().is_empty(), "too soon");
    app.tick(&mut ui, 6000);
    app.render(&mut canvas, &mut ui, 6000);
    app.handle(press(Button::A), &mut ui);
    assert_eq!(
        app.data.demo_answers(),
        [("prompt:demo".to_owned(), "yes".to_owned())]
    );
}

/// When a question is over (its turn ended, or Hermes took it back) the card goes, and with
/// it its answers: a press on its way answers nothing, and the buttons mean what they did
/// before. On a Hermes, Y lists the commands again.
#[test]
fn a_card_that_is_taken_away_takes_its_answers_with_it() {
    let (mut app, mut ui, mut canvas) =
        chat_with_request("request-approval", (1024, 768), Default::default());
    app.data.set_demo_scenario("request-over");
    for button in [Button::A, Button::Y, Button::B] {
        app.handle(press(button), &mut ui);
        app.tick(&mut ui, 2016);
        app.render(&mut canvas, &mut ui, 2016);
        // Whatever the press opened is closed again.
        app.handle(press(Button::B), &mut ui);
    }
    assert!(app.data.demo_answers().is_empty());
    assert!(app.request_card().is_none());
    // Y then A: the first of the commands, which only shows something.
    let before = app.data.main_chat().len();
    app.tick(&mut ui, 2032);
    app.render(&mut canvas, &mut ui, 2032);
    app.handle(press(Button::Y), &mut ui);
    app.handle(press(Button::A), &mut ui);
    let sent: Vec<String> = app
        .data
        .main_chat()
        .iter()
        .skip(before)
        .filter(|t| t.from_user)
        .map(|t| t.body.clone())
        .collect();
    assert_eq!(sent, ["/status"]);
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
        // A asks which kind of agent first; nothing is opened until that is answered.
        assert!(app.data.pairing().is_none());
        if pairing {
            app.handle(press(Button::A), &mut ui);
        }
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

/// Pairing begins with which kind of agent: Muse on the left, a Hermes Agent on the right.
#[test]
fn pairing_asks_which_agent_first() {
    for (side, hermes) in [(Button::Left, false), (Button::Right, true)] {
        let mut ui = Ui::new().unwrap();
        let mut app = App::new(Theme::new(Mode::Light, 2.0));
        app.data.set_demo_scenario("unpaired");
        render_once(&mut app, &mut ui, 1000);
        render_once(&mut app, &mut ui, 1016);
        app.handle(press(Button::A), &mut ui);
        app.handle(press(side), &mut ui);
        app.handle(press(Button::A), &mut ui);
        let pairing = app.data.pairing().expect("pairing opened");
        assert_eq!(pairing.hermes, hermes, "{side:?}");
        render_once(&mut app, &mut ui, 1032);
    }
    // B at the question leaves everything as it was.
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    assert!(app.force_screen("provider"));
    app.handle(press(Button::B), &mut ui);
    app.handle(press(Button::A), &mut ui);
    assert!(app.data.pairing().is_none());
}

/// With a Hermes Agent, Y lists a few of its commands and A sends the one chosen; there is
/// nothing to call. With Muse, Y does nothing in the chat and F2 calls.
#[test]
fn y_offers_a_hermes_its_commands_and_nothing_is_called() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    assert!(app.force_screen("chat"));
    render_once(&mut app, &mut ui, 1000);
    let before = app.data.main_chat().len();
    app.handle(press(Button::Y), &mut ui);
    app.handle(press(Button::A), &mut ui);
    assert!(
        !app.data
            .main_chat()
            .iter()
            .skip(before)
            .any(|t| t.body.starts_with('/')),
        "Muse is sent no command"
    );

    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    assert!(app.force_screen("chat@hermes"));
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::F2), &mut ui);
    assert!(app.data.call().is_none(), "a Hermes has no calls");
    let before = app.data.main_chat().len();
    // Y, down to the second row, A.
    app.handle(press(Button::Y), &mut ui);
    render_once(&mut app, &mut ui, 1016);
    assert_eq!(
        app.data.main_chat().len(),
        before,
        "opening the list sends nothing"
    );
    app.handle(press(Button::Down), &mut ui);
    app.handle(press(Button::A), &mut ui);
    let sent: Vec<String> = app
        .data
        .main_chat()
        .iter()
        .skip(before)
        .filter(|t| t.from_user)
        .map(|t| t.body.clone())
        .collect();
    assert_eq!(sent, ["/model"]);
    // The first row only shows something: Y and then A by mistake starts nothing over.
    let before = app.data.main_chat().len();
    app.handle(press(Button::Y), &mut ui);
    app.handle(press(Button::A), &mut ui);
    let first: Vec<String> = app
        .data
        .main_chat()
        .iter()
        .skip(before)
        .filter(|t| t.from_user)
        .map(|t| t.body.clone())
        .collect();
    assert_eq!(first, ["/status"]);
    // B closes the list without sending.
    let before = app.data.main_chat().len();
    app.handle(press(Button::Y), &mut ui);
    app.handle(press(Button::B), &mut ui);
    assert_eq!(app.data.main_chat().len(), before);
}

/// While a request waits in a chat with a Hermes, Y belongs to its answers: no list of
/// commands opens over it, and nothing is sent.
#[test]
fn a_waiting_request_keeps_y_from_the_commands() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    app.data.set_demo_scenario("request");
    app.data.set_demo_scenario("hermes");
    render_once(&mut app, &mut ui, 1000);
    render_once(&mut app, &mut ui, 3000);
    let before = app.data.main_chat().len();
    app.handle(press(Button::Y), &mut ui);
    // Had the list opened, A would choose its first row and send that command. Over the
    // request, A is the request's own answer instead.
    app.handle(press(Button::A), &mut ui);
    render_once(&mut app, &mut ui, 3016);
    let sent: Vec<String> = app
        .data
        .main_chat()
        .iter()
        .skip(before)
        .filter(|t| t.from_user)
        .map(|t| t.body.clone())
        .collect();
    assert!(sent.iter().all(|t| !t.starts_with('/')), "sent {sent:?}");
}

/// Trying again after a pairing failed is with the same kind of agent.
#[test]
fn retrying_a_failed_pairing_keeps_its_agent() {
    for (screen, hermes) in [("pairing-failed", false), ("pairing-hermes-failed", true)] {
        let mut ui = Ui::new().unwrap();
        let mut app = App::new(Theme::new(Mode::Light, 2.0));
        assert!(app.force_screen(screen));
        render_once(&mut app, &mut ui, 1000);
        app.handle(press(Button::A), &mut ui);
        let pairing = app.data.pairing().expect("pairing again");
        assert!(!pairing.step.ended(), "{screen}");
        assert_eq!(pairing.hermes, hermes, "{screen}");
    }
}

/// While the handheld connects to a Hermes there is one thing to do: stop it, with B.
#[test]
fn a_pairing_that_is_connecting_is_stopped_with_b() {
    use goblin_ui::data::PairStep;
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    assert!(app.force_screen("pairing-hermes-connecting"));
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::A), &mut ui);
    let step = app.data.pairing().map(|p| p.step);
    assert_eq!(step, Some(PairStep::Connecting), "A leaves it be");
    app.handle(press(Button::B), &mut ui);
    assert!(app.data.pairing().is_none());
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

/// Select over the chat list shows its buttons; the next press puts only that away, and
/// the list is still open on the row it was on.
#[test]
fn controls_over_the_chat_list_leave_the_list_open() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    app.reduce_motion = true;
    assert!(app.force_screen("chat"));
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::Left), &mut ui);
    render_once(&mut app, &mut ui, 1010);
    app.handle(press(Button::Down), &mut ui);
    app.handle(press(Button::Down), &mut ui);
    assert!(app.panel_focus_is_side_chat());
    app.handle(press(Button::Select), &mut ui);
    render_once(&mut app, &mut ui, 1020);
    app.handle(press(Button::Select), &mut ui);
    render_once(&mut app, &mut ui, 1030);
    assert!(
        app.panel_focus_is_side_chat(),
        "the list is open where it was"
    );
    // And it still works: A opens the chat that was selected.
    app.handle(press(Button::A), &mut ui);
    render_once(&mut app, &mut ui, 1040);
    assert_eq!(app.data.active_chat(), "side-1");
}

#[test]
fn leaving_the_app_ends_its_call() {
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    let mut ui = Ui::new().unwrap();
    app.force_screen("chat");
    app.handle(press(Button::F2), &mut ui);
    assert!(app.data.call().is_some());
    app.handle(press(Button::Menu), &mut ui);
    render_once(&mut app, &mut ui, 1000);
    app.handle(press(Button::A), &mut ui);
    assert!(app.quit);
    assert!(app.data.call().is_none());
}

#[test]
fn owner_can_toggle_writes_outside_home_in_settings() {
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    app.data.set_demo_scenario("newprofile");
    assert!(app.force_screen("settings-access"));
    render_once(&mut app, &mut ui, 1000);
    render_once(&mut app, &mut ui, 1000);
    for _ in 0..3 {
        app.handle(press(Button::Down), &mut ui);
        render_once(&mut app, &mut ui, 1000);
    }
    assert!(!app.data.consent().file_write_outside_home);
    app.handle(press(Button::A), &mut ui);
    assert!(app.data.consent().file_write_outside_home);
    app.handle(press(Button::A), &mut ui);
    assert!(!app.data.consent().file_write_outside_home);
}

#[test]
fn reply_pills_navigate_horizontally_without_jumping_to_the_composer() {
    for sprites in [false, true] {
        let mut ui = Ui::new().unwrap();
        let mut app = App::new(Theme::new(Mode::Light, 2.0));
        app.reduce_motion = true;
        app.force_screen("chat");
        app.data.set_demo_quick_replies(vec![
            "Put it on the Brick".into(),
            "Make it a web page".into(),
            "Surprise me".into(),
        ]);
        ui.sprites.begin(sprites, app.sprite_salt());
        render_once(&mut app, &mut ui, 1000);
        assert!(ui.focus.contains(102));
        let messages = app.data.main_chat().len();
        ui.focus.set(1);
        for (button, expected) in [
            (Button::Right, 1),
            (Button::Up, 102),
            (Button::Left, 101),
            (Button::Left, 100),
            (Button::Right, 101),
            (Button::Right, 102),
            (Button::Right, 102),
            (Button::Down, 1),
        ] {
            app.handle(press(button), &mut ui);
            ui.sprites.begin(sprites, app.sprite_salt());
            render_once(&mut app, &mut ui, 1000);
            assert_eq!(
                ui.focus.current(),
                Some(expected),
                "{button:?}, sprites={sprites}"
            );
        }
        assert_eq!(
            app.data.main_chat().len(),
            messages,
            "navigation must not send"
        );
    }
}

#[test]
fn failed_pairing_can_retry_the_same_profile_or_go_back() {
    use goblin_ui::data::PairStep;
    let mut ui = Ui::new().unwrap();
    let mut app = App::new(Theme::new(Mode::Light, 2.0));
    assert!(app.force_screen("pairing-failed"));
    app.handle(press(Button::A), &mut ui);
    let retry = app.data.pairing().unwrap();
    assert_eq!(retry.profile, "work");
    assert_eq!(retry.step, PairStep::Waiting);
    app.handle(press(Button::B), &mut ui);
    assert!(app.data.pairing().is_none());
    assert!(app.force_screen("pairing-failed"));
    app.handle(press(Button::B), &mut ui);
    assert!(app.data.pairing().is_none());
}
