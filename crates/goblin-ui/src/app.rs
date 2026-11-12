//! Application state and the screen router.
//!
//! [`App`] owns the theme, the mock data provider, the navigation stack and all transient UI
//! state (composer, keyboard, overlays, toast). It is rendered head-lessly into a
//! [`goblin_ui_kit::Canvas`] by [`App::render`], which the SDL shell and the screenshot/golden
//! paths all share. Input arrives as [`goblin_ui_kit::InputEvent`]s through [`App::handle`].

use goblin_ui_kit::widgets::{
    ConfirmDialog, Header, Hints, KeyboardState, LinkState, ListRow, Painter, TabItem, TextRole,
    Toast, ToastKind,
};
use goblin_ui_kit::{
    AvatarSize, Button, Canvas, Color, FocusId, Icon, InputEvent, Mode, Phase, Rect, Theme, Ui,
};

use crate::data::{ConsentKind, DataProvider, MockProvider, MAIN_CHAT};

mod screens;

/// A screen in the router.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    /// Main chat.
    Chat,
    /// Feed (account mode).
    Feed,
    /// Ideas (account mode).
    Ideas,
    /// Goals (account mode).
    Goals,
    /// Library (account mode).
    Library,
    /// Side chats list.
    SideChats,
    /// Notifications inbox.
    Notifications,
    /// Device status.
    Status,
    /// Settings root.
    Settings,
    /// Appearance settings.
    Appearance,
    /// Input tester.
    InputTest,
}

impl Screen {
    fn key(self) -> u64 {
        self as u64
    }

    /// The screen's name, shown top-left on screens that are not tabs.
    fn title(self) -> &'static str {
        match self {
            Screen::SideChats => "Side chats",
            Screen::Notifications => "Notifications",
            Screen::Status => "Status",
            Screen::Settings => "Settings",
            Screen::Appearance => "Appearance",
            Screen::InputTest => "Input test",
            _ => "",
        }
    }

    /// Whether the screen is a top-level tab.
    fn is_tab(self) -> bool {
        matches!(
            self,
            Screen::Chat | Screen::Feed | Screen::Ideas | Screen::Goals | Screen::Library
        )
    }
}

/// An active overlay on top of the current screen.
#[derive(Clone, Copy)]
enum Overlay {
    None,
    Voice {
        dictation: bool,
        level: f32,
        start_ms: u64,
    },
    Confirm {
        title: &'static str,
        message: &'static str,
        confirm: &'static str,
        danger: bool,
        focus: u8,
        /// What confirming does.
        then: ConfirmAction,
    },
    /// Full-screen picture and clip viewer over the chat's media.
    Viewer {
        index: usize,
    },
    /// The navigation panel: slides in from the left, as in the Muse app.
    Panel {
        focus: usize,
    },
    /// What can be done with the profile named in `App::sheet_profile`. `armed` is the row
    /// that asked to be pressed again (unpair, delete).
    Profile {
        focus: usize,
        armed: Option<usize>,
    },
    /// Pairing a profile with the Muse app (the state is the provider's).
    Pairing,
}

/// What the keyboard is naming, while it holds a name instead of a message.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RenameTarget {
    /// A side chat, by id.
    Chat(String),
    /// A profile, by its present name.
    Profile(String),
}

/// A profile name from what was typed: spaces become hyphens, and it must then be 1 to 32
/// letters, digits, hyphens or underscores (it names a folder on the card).
fn profile_name_from(typed: &str) -> Result<String, &'static str> {
    let name: String = typed
        .trim()
        .chars()
        .map(|c| if c == ' ' { '-' } else { c })
        .collect();
    let ok = !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(name)
    } else {
        Err("Use up to 32 letters, digits, - or _")
    }
}

/// One thing the profile sheet offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProfileAction {
    Switch,
    Rename,
    Pair,
    Unpair,
    Delete,
}

/// The profile sheet's rows for `p`.
fn profile_actions(p: &crate::data::ProfileEntry) -> Vec<(ProfileAction, &'static str)> {
    let mut out = Vec::new();
    if !p.active {
        out.push((ProfileAction::Switch, "Use this profile"));
    }
    out.push((
        ProfileAction::Pair,
        if p.paired {
            "Pair again with the Muse app"
        } else {
            "Pair with the Muse app"
        },
    ));
    out.push((ProfileAction::Rename, "Rename"));
    if p.paired {
        out.push((ProfileAction::Unpair, "Unpair from this handheld"));
    }
    if !p.active {
        out.push((ProfileAction::Delete, "Delete this profile"));
    }
    out
}

/// What a confirmation dialog does when confirmed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConfirmAction {
    /// Leave the app (the daemon keeps running).
    Quit,
    /// Forget the side chat at this index (on this device only).
    ForgetChat(usize),
    /// Nothing (demo dialogs).
    Nothing,
}

/// One row of the navigation panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PanelItem {
    /// A tab (index into the visible tabs); Chat means the main chat.
    Tab(usize),
    /// Start a side chat.
    NewChat,
    /// A side chat (index into the provider's list).
    Side(usize),
    Inbox,
    Status,
    Settings,
    InputTest,
    Quit,
}

/// How long a new message takes to rise in.
const APPEAR_MS: u64 = 180;

/// How long the panel takes to slide in and out (the app's is about 230 ms).
const PANEL_IN_MS: u32 = 180;
const PANEL_OUT_MS: u32 = 130;

/// A transient toast with an expiry time.
struct ActiveToast {
    kind: ToastKind,
    title: String,
    body: String,
    expires_ms: u64,
}

/// The application.
pub struct App {
    /// The active theme (re-themed live from Settings).
    pub theme: Theme,
    /// The data provider (mock for demos and tests, live IPC on the device).
    pub data: Box<dyn DataProvider>,
    /// Index into the visible tab list.
    tab: usize,
    /// Router stack above the active tab.
    stack: Vec<Screen>,
    /// Scroll offset per screen key.
    scroll: Vec<(u64, f32)>,
    /// Content height measured last render (for scroll clamping) per screen key.
    content_h: Vec<(u64, f32)>,
    /// Composer text.
    composer: String,
    /// Whether the on-screen keyboard is open.
    keyboard_open: bool,
    /// Renaming a side chat: its id, and the message draft set aside while the keyboard
    /// holds the title.
    renaming: Option<(RenameTarget, String)>,
    /// Keyboard state.
    kb: KeyboardState,
    /// Active overlay.
    overlay: Overlay,
    /// Active toast.
    toast: Option<ActiveToast>,
    /// Screen pinned to its bottom: the next render scrolls to the newly measured end.
    pin_bottom: Option<u64>,
    /// Older messages were put in front of the chat: the scroll range it had before, so the
    /// next render keeps the same messages on screen.
    hold_view: Option<f32>,
    /// The chat is near its oldest message on screen: ask for older ones on the next tick.
    want_older: bool,
    /// Whether the agent was busy at the last tick (to switch the mascot on changes only).
    was_busy: bool,
    /// The agent's own avatar, once the daemon has prepared it.
    avatar: Option<crate::media::AvatarAssets>,
    /// The avatar frame shown last tick (a change means a redraw).
    avatar_frame_seen: Option<usize>,
    /// Decoded pictures for media tiles and the viewer.
    pics: crate::media::PictureCache,
    /// The profile the profile sheet is about.
    sheet_profile: String,
    /// The focused Settings row opens something (a profile) instead of changing in place.
    settings_focus_opens: bool,
    /// The media the viewer steps through (the open chat's, in order).
    viewer_items: Vec<crate::media::Media>,
    /// The clip being played in the viewer, and its newest frame.
    player: Option<crate::media::ClipPlayer>,
    player_frame: Option<goblin_ui_kit::Image>,
    /// The clip in the viewer is paused at this many seconds in (its last frame stays up).
    clip_paused: Option<f32>,
    /// How far the playing clip has got, and its length once known, both in seconds.
    clip_pos: f32,
    clip_len: Option<std::sync::mpsc::Receiver<Option<f32>>>,
    clip_secs: Option<f32>,
    /// The focus ring is mid-glide (keeps frames coming).
    ring_moving: bool,
    last_tick_ms: u64,
    /// Scroll positions as drawn: they ease toward the targets in `scroll`.
    scroll_shown: Vec<(u64, f32)>,
    /// When the newest message appeared, and how many the chat had before it (for its
    /// rise-in).
    appear: Option<(u64, usize)>,
    turns_seen: usize,
    chat_seen: String,
    /// The avatar's growth during a call: 0 normal, 1 call size.
    call_avatar: goblin_ui_kit::anim::Tween,
    /// The reply choices last seen, to move focus to them when Muse offers new ones.
    pills_seen: String,
    /// Focus to take on the next frame.
    focus_request: Option<FocusId>,
    /// The screen and item whose focus was last brought into view, and whether the view is
    /// still on its way there.
    focus_scrolled: Option<(u64, FocusId)>,
    focus_settling: bool,
    /// The provider's settings revision last applied to the theme.
    settings_seen: u64,
    /// The panel's slide: 0 closed, 1 open.
    panel_anim: goblin_ui_kit::anim::Tween,
    /// The panel is sliding out; the overlay closes when it has.
    panel_closing: bool,
    /// The slide is in progress this frame (keeps the loop drawing).
    panel_moving: bool,
    /// What to do once the panel has closed (activating a row closes first).
    panel_then: Option<PanelItem>,
    /// Height of the floating header band over the chat (set each frame).
    chat_top_inset: f32,
    /// Height of the body area last frame (for paging).
    body_h: f32,
    /// System volume (NextUI's, changed by the volume keys), polled; `None` off NextUI.
    sysvol: Option<goblin_platform::sysvol::SystemVolume>,
    /// When the volume was last polled, and until when its bar shows.
    sysvol_polled_ms: u64,
    sysvol_hud_until_ms: u64,
    /// Read the system volume (off in tests and screenshots).
    pub watch_system_volume: bool,
    /// Settings: reduce motion.
    pub reduce_motion: bool,
    /// Whether to draw the FPS overlay.
    pub show_fps: bool,
    /// Measured frame time in ms (for the overlay).
    pub frame_ms: f32,
    /// Where the last frame's time went, in ms: before the body (theme, avatar, header
    /// band), the body, the header and hint bar, and everything after (overlays, ring).
    pub phase_ms: [f32; 4],
    /// Input-test: currently held buttons.
    pub held: Vec<Button>,
    /// Input-test: last pressed button.
    pub last_pressed: Option<Button>,
    /// Quit requested.
    pub quit: bool,
    /// Whether a redraw is needed.
    dirty: bool,
    /// Draw a fixed clock (golden images and screenshots stay reproducible).
    pub fixed_clock: bool,
    /// The current animation clock.
    now_ms: u64,
    /// Whether an animation is currently running (keeps the loop awake).
    /// The screen key rendered last frame, so focus memory only restores on screen entry.
    last_key: Option<u64>,
}

/// How often the system volume is read, and how long its bar shows after a change.
const SYSVOL_POLL_MS: u64 = 150;
const SYSVOL_HUD_MS: u64 = 1500;

impl App {
    /// Builds the app with the mock provider.
    pub fn new(theme: Theme) -> Self {
        Self::with_provider(theme, Box::new(MockProvider::new()))
    }

    /// Builds the app over any data provider.
    pub fn with_provider(theme: Theme, data: Box<dyn DataProvider>) -> Self {
        App {
            theme,
            data,
            tab: 0,
            stack: Vec::new(),
            scroll: Vec::new(),
            content_h: Vec::new(),
            composer: String::new(),
            keyboard_open: false,
            renaming: None,
            kb: KeyboardState::default(),
            overlay: Overlay::None,
            toast: None,
            was_busy: false,
            avatar: None,
            avatar_frame_seen: None,
            pics: crate::media::PictureCache::default(),
            sheet_profile: String::new(),
            settings_focus_opens: false,
            viewer_items: Vec::new(),
            player: None,
            player_frame: None,
            clip_paused: None,
            clip_pos: 0.0,
            clip_len: None,
            clip_secs: None,
            ring_moving: false,
            last_tick_ms: 0,
            scroll_shown: Vec::new(),
            appear: None,
            turns_seen: 0,
            chat_seen: String::new(),
            call_avatar: goblin_ui_kit::anim::Tween::new(0.0),
            pills_seen: String::new(),
            focus_request: None,
            focus_scrolled: None,
            focus_settling: false,
            settings_seen: 0,
            panel_anim: goblin_ui_kit::anim::Tween::new(0.0),
            panel_closing: false,
            panel_moving: false,
            panel_then: None,
            chat_top_inset: 0.0,
            body_h: 0.0,
            sysvol: None,
            sysvol_polled_ms: 0,
            sysvol_hud_until_ms: 0,
            watch_system_volume: false,
            pin_bottom: None,
            hold_view: None,
            want_older: false,
            reduce_motion: false,
            show_fps: false,
            frame_ms: 0.0,
            phase_ms: [0.0; 4],
            held: Vec::new(),
            last_pressed: None,
            quit: false,
            dirty: true,
            fixed_clock: true,
            now_ms: 0,
            last_key: None,
        }
    }

    /// The visible tabs given the account-linked state.
    fn tabs(&self) -> Vec<(Screen, TabItem)> {
        let mut v = vec![(
            Screen::Chat,
            TabItem {
                icon: Icon::Chat,
                label: "Chat",
            },
        )];
        if self.data.account_linked() {
            v.push((
                Screen::Feed,
                TabItem {
                    icon: Icon::Feed,
                    label: "Feed",
                },
            ));
            v.push((
                Screen::Ideas,
                TabItem {
                    icon: Icon::Ideas,
                    label: "Ideas",
                },
            ));
            v.push((
                Screen::Goals,
                TabItem {
                    icon: Icon::Goals,
                    label: "Goals",
                },
            ));
            v.push((
                Screen::Library,
                TabItem {
                    icon: Icon::Library,
                    label: "Library",
                },
            ));
        }
        v
    }

    /// The currently active screen (top of stack, or the active tab).
    fn active(&self) -> Screen {
        self.stack
            .last()
            .copied()
            .unwrap_or_else(|| self.tabs()[self.tab.min(self.tabs().len() - 1)].0)
    }

    /// Whether the UI is currently animating, so the shell keeps rendering at vsync. When this is
    /// false the shell blocks on input and idle CPU falls to near zero; the mascot animates only
    /// in its active states (listening / thinking / speaking), which all coincide with one of the
    /// conditions below, so the top-bar avatar is intentionally static while idle.
    pub fn is_animating(&self) -> bool {
        self.data.animating()
            || self.panel_moving
            || matches!(self.overlay, Overlay::Voice { .. })
            || self.call_avatar.target() != self.call_avatar.value()
            || self.ring_moving
            || self.player.is_some()
            || self.scroll_easing()
            || self.appear.is_some()
    }

    /// Whether the navigation panel is open with a side chat focused (tests).
    pub fn panel_focus_is_side_chat(&self) -> bool {
        match self.overlay {
            Overlay::Panel { focus } => {
                matches!(self.panel_items().get(focus), Some(PanelItem::Side(_)))
            }
            _ => false,
        }
    }

    /// Whether the on-screen keyboard is open.
    pub fn keyboard_open(&self) -> bool {
        self.keyboard_open
    }

    /// Whether the voice-note recorder overlay is open.
    pub fn recording_voice(&self) -> bool {
        matches!(
            self.overlay,
            Overlay::Voice {
                dictation: false,
                ..
            }
        )
    }

    /// Decodes pictures on a worker thread instead of while drawing (the app does; the
    /// screenshots and tests leave it off so a frame is complete the first time).
    pub fn decode_pictures_in_background(&mut self) {
        self.pics.decode_in_background();
    }

    /// Whether the frame about to be drawn can be made of sprites alone (see
    /// `goblin_ui_kit::sprites`): the chat as it is while being read, with nothing on it that
    /// moves by itself or that is still drawn in place. The shell asks before each frame and
    /// checks afterwards that nothing was drawn outside a sprite; when in doubt this says no,
    /// and the frame is drawn the plain way.
    pub fn sprite_frame(&self) -> bool {
        self.active() == Screen::Chat
            && matches!(self.overlay, Overlay::None)
            && !self.keyboard_open
            && self.toast.is_none()
            && self.sysvol_hud_until_ms == 0
            && !self.data.speaking()
            && self.appear.is_none()
            && self.data.call().is_none()
            && self.data.mic_level().is_none()
            && !self.data.agent_busy()
            && self.data.status().link == LinkState::Connected
            && !self.data.main_chat().is_empty()
            && self.avatar.as_ref().is_some_and(|a| a.still.is_some())
            && !self.show_fps
    }

    /// What every sprite's look depends on besides its own content.
    pub fn sprite_salt(&self) -> u64 {
        goblin_ui_kit::widgets::sprite_key(&(
            self.theme.mode == Mode::Dark,
            self.theme.chat_theme.id,
            self.theme.scale.to_bits(),
            self.theme.metrics.body.to_bits(),
            self.theme.metrics.row.to_bits(),
            self.theme.avatar.diameter().to_bits(),
            self.reduce_motion,
        ))
    }

    /// Whether a redraw is needed since the last render.
    pub fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn scroll_of(&self, key: u64) -> f32 {
        self.scroll
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
            .unwrap_or(0.0)
    }

    /// The scroll position to draw: it eases toward the target set by `set_scroll`.
    fn scroll_shown_of(&self, key: u64) -> f32 {
        self.scroll_shown
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
            .unwrap_or_else(|| self.scroll_of(key))
    }

    /// Whether any drawn scroll position is still catching up with its target.
    fn scroll_easing(&self) -> bool {
        self.scroll_shown
            .iter()
            .any(|(k, v)| (self.scroll_of(*k) - v).abs() > 0.5)
    }

    /// Moves drawn scroll positions toward their targets: about two thirds of the remaining
    /// distance every 60 ms, so a page settles in roughly a fifth of a second. Returns true
    /// while anything moved.
    fn ease_scroll(&mut self, dt_ms: u64) -> bool {
        let instant = self.reduce_motion;
        let k = 1.0 - 0.33f32.powf(dt_ms.min(200) as f32 / 60.0);
        let mut moved = false;
        for i in 0..self.scroll.len() {
            let (key, target) = self.scroll[i];
            let shown = match self.scroll_shown.iter_mut().find(|(kk, _)| *kk == key) {
                Some(e) => &mut e.1,
                None => {
                    self.scroll_shown.push((key, target));
                    continue;
                }
            };
            let diff = target - *shown;
            if diff.abs() <= 0.5 {
                *shown = target;
            } else if instant {
                *shown = target;
                moved = true;
            } else {
                *shown += diff * k;
                moved = true;
            }
        }
        moved
    }

    fn set_scroll(&mut self, key: u64, v: f32) {
        if let Some(e) = self.scroll.iter_mut().find(|(k, _)| *k == key) {
            e.1 = v;
        } else {
            self.scroll.push((key, v));
        }
    }

    fn set_content_h(&mut self, key: u64, v: f32) {
        if let Some(e) = self.content_h.iter_mut().find(|(k, _)| *k == key) {
            e.1 = v;
        } else {
            self.content_h.push((key, v));
        }
    }

    fn content_h_of(&self, key: u64) -> f32 {
        self.content_h
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
            .unwrap_or(0.0)
    }

    fn push(&mut self, s: Screen) {
        self.stack.push(s);
        self.mark_dirty();
    }

    fn pop(&mut self) {
        // Leaving the inbox marks what was shown as seen (unseen rows stay highlighted while open).
        if self.stack.pop() == Some(Screen::Notifications) {
            self.data.mark_notifications_seen();
        }
        self.mark_dirty();
    }

    /// Whether the chat is scrolled back more than half a screen from its newest message
    /// (the same test that draws the "Newest" cue).
    fn chat_scrolled_back(&self) -> bool {
        if self.active() != Screen::Chat {
            return false;
        }
        let key = Screen::Chat.key();
        self.content_h_of(key) - self.scroll_of(key) > self.body_h * 0.5
    }

    /// Which avatar clip plays now: "working" while Muse works, "idle" during a call, and
    /// none at rest (the still picture shows).
    fn avatar_state(&self) -> Option<&'static str> {
        if self.data.agent_busy() {
            Some("working")
        } else if self.data.call().is_some() {
            Some("idle")
        } else {
            None
        }
    }

    /// Opens a chat (main or side) in the Chat tab, pinned to its newest message.
    fn open_chat(&mut self, id: &str) {
        self.data.open_chat(id);
        self.show_chat_tab();
    }

    /// Returns to the Chat tab, pinned to its newest message.
    fn show_chat_tab(&mut self) {
        while !self.stack.is_empty() {
            self.pop();
        }
        if let Some(i) = self.tabs().iter().position(|(s, _)| *s == Screen::Chat) {
            self.tab = i;
        }
        self.autoscroll_bottom(Screen::Chat.key());
        self.mark_dirty();
    }

    fn show_toast(&mut self, kind: ToastKind, title: &str, body: &str) {
        self.toast = Some(ActiveToast {
            kind,
            title: title.into(),
            body: body.into(),
            expires_ms: self.now_ms + 2600,
        });
        self.mark_dirty();
    }

    /// Advances time-based state (reply streaming, toast expiry). Call once per frame before
    /// rendering. Returns true if something changed.
    pub fn tick(&mut self, ui: &mut Ui, now_ms: u64) -> bool {
        self.now_ms = now_ms;
        let mut changed = false;
        if self.data.pump(now_ms) {
            let older = self.data.take_older_added();
            if older > 0 {
                // Older messages went in above: keep what is on screen where it is (the
                // next render moves the scroll by the height they add), and do not treat
                // them as a message that has just arrived.
                let key = Screen::Chat.key();
                self.hold_view = Some(self.content_h_of(key));
                self.turns_seen += older;
                // Media tiles are numbered in order: a focused one keeps its focus.
                let media: u64 = self
                    .data
                    .main_chat()
                    .iter()
                    .take(older)
                    .map(|t| t.media.len() as u64)
                    .sum();
                if let Some(id) = ui
                    .focus
                    .current()
                    .filter(|id| media > 0 && *id >= screens::MEDIA_FOCUS_BASE)
                {
                    self.focus_request = Some(id + media);
                }
                if let Some((_, from)) = self.appear.as_mut() {
                    *from += older;
                }
            } else if !self.chat_scrolled_back() {
                // Keep the chat at its newest message while replies arrive, unless the
                // owner is reading further back.
                self.autoscroll_bottom(Screen::Chat.key());
            }
            changed = true;
        }
        if std::mem::take(&mut self.want_older) && self.data.load_older() {
            changed = true;
        }
        // A picture finished decoding: its tile can show it now.
        if self.pics.poll() {
            self.mark_dirty();
            changed = true;
        }
        let busy = self.data.agent_busy();
        if busy != self.was_busy {
            self.was_busy = busy;
            ui.mascot.set_state(if busy {
                goblin_ui_kit::MascotState::Thinking
            } else {
                goblin_ui_kit::MascotState::Idle
            });
            changed = true;
        }
        if let Some(t) = &self.toast {
            if now_ms >= t.expires_ms {
                self.toast = None;
                changed = true;
            }
        }
        // The agent's avatar: load it when the daemon says where it is (or it changed), and
        // step its clip while Muse is working or on a call. At rest it is a still picture, so
        // nothing redraws.
        let dir = self.data.avatar_dir();
        if dir.as_deref() != self.avatar.as_ref().map(|a| a.dir.as_str()) {
            self.avatar = dir.as_deref().and_then(crate::media::AvatarAssets::load);
            changed = true;
        }
        let frame = match (&self.avatar, self.avatar_state()) {
            (Some(a), Some(state)) if !self.reduce_motion => a.frame_index(state, now_ms),
            _ => None,
        };
        if frame != self.avatar_frame_seen {
            self.avatar_frame_seen = frame;
            changed = true;
        }
        // A clip playing in the viewer: show each frame as it arrives; stop at the end.
        if let Some(player) = self.player.as_mut() {
            if let Some(frame) = player.take_frame() {
                self.player_frame = Some(frame);
                self.clip_pos = player.position();
                changed = true;
            }
            if player.finished() {
                self.stop_clip();
                changed = true;
            }
        }
        if let Some(rx) = &self.clip_len {
            if let Ok(secs) = rx.try_recv() {
                self.clip_secs = secs;
                self.clip_len = None;
                changed = true;
            }
        }
        // Scrolling eases to its target.
        let dt = now_ms.saturating_sub(self.last_tick_ms);
        self.last_tick_ms = now_ms;
        if self.ease_scroll(dt) {
            changed = true;
        }
        // A message that has just arrived rises in (not when a different chat is opened).
        let n = self.data.main_chat().len();
        let chat = self.data.active_chat().to_owned();
        if chat != self.chat_seen {
            self.chat_seen = chat;
            self.turns_seen = n;
            self.appear = None;
        } else if n > self.turns_seen {
            if !self.reduce_motion {
                self.appear = Some((now_ms, self.turns_seen));
            }
            self.turns_seen = n;
            changed = true;
        } else if n < self.turns_seen {
            self.turns_seen = n;
        }
        if let Some((start, _)) = self.appear {
            if now_ms.saturating_sub(start) >= APPEAR_MS {
                self.appear = None;
            }
            changed = true;
        }
        // New reply choices: the first one takes focus, so answering is one press of A.
        let pills = self.data.quick_replies().join("\u{1f}");
        if pills != self.pills_seen {
            self.pills_seen = pills;
            if !self.pills_seen.is_empty() && !self.keyboard_open {
                ui.memory.set(Screen::Chat.key(), 100);
                if self.active() == Screen::Chat {
                    self.focus_request = Some(100);
                }
                changed = true;
            }
        }
        // Settings changed (here or from the daemon): re-apply size, mode, colours.
        if self.data.settings_rev() != self.settings_seen {
            self.apply_settings();
            changed = true;
        }
        // The panel's slide.
        self.panel_moving = self.panel_anim.tick(now_ms);
        if self.panel_moving {
            changed = true;
        } else if self.panel_closing {
            self.panel_closing = false;
            self.overlay = Overlay::None;
            if let Some(item) = self.panel_then.take() {
                self.run_panel(item);
            }
            changed = true;
        }
        // A call runs inside the chat: the avatar grows while it lasts and follows its state.
        // An ending is quiet unless something went wrong.
        if let Some(reason) = self.data.take_call_end() {
            ui.mascot.set_state(goblin_ui_kit::MascotState::Idle);
            match reason.as_str() {
                "local_end" | "remote_stop" | "remote_end" => {}
                "idle_timeout" => {
                    self.show_toast(ToastKind::Info, "Call ended", "No one spoke for a while")
                }
                other => self.show_toast(ToastKind::Error, "Call failed", other),
            }
            changed = true;
        } else if let Some(call) = self.data.call() {
            use crate::data::CallPhase;
            let want = match call.phase {
                CallPhase::Connecting | CallPhase::Thinking => goblin_ui_kit::MascotState::Thinking,
                CallPhase::Listening => goblin_ui_kit::MascotState::Listening,
                CallPhase::Speaking => goblin_ui_kit::MascotState::Speaking,
            };
            if ui.mascot.state() != want {
                ui.mascot.set_state(want);
                changed = true;
            }
        }
        let on_call = self.data.call().is_some();
        let grow = if on_call { 1.0 } else { 0.0 };
        if (self.call_avatar.target() - grow).abs() > f32::EPSILON {
            if self.reduce_motion {
                self.call_avatar.snap(grow);
            } else {
                self.call_avatar.set(grow, 320, now_ms);
            }
            changed = true;
        }
        if self.call_avatar.tick(now_ms) {
            changed = true;
        }
        if self.watch_system_volume && now_ms >= self.sysvol_polled_ms + SYSVOL_POLL_MS {
            self.sysvol_polled_ms = now_ms;
            let v = goblin_platform::sysvol::read_nextui();
            if v != self.sysvol {
                // Show the bar on a change (not for the first reading).
                if self.sysvol.is_some() {
                    self.sysvol_hud_until_ms = now_ms + SYSVOL_HUD_MS;
                }
                self.sysvol = v;
                changed = true;
            }
        }
        if self.sysvol_hud_until_ms != 0 && now_ms >= self.sysvol_hud_until_ms {
            self.sysvol_hud_until_ms = 0;
            changed = true;
        }
        if let Overlay::Voice { level, .. } = &mut self.overlay {
            // The real microphone level when the provider measures it (smoothed and lifted,
            // since speech peaks are small); a gentle wave otherwise.
            *level = match self.data.mic_level() {
                Some(v) => (*level * 0.5 + (v * 4.0).min(1.0) * 0.5).clamp(0.04, 1.0),
                None => 0.35 + 0.35 * ((now_ms as f32 / 140.0).sin() * 0.5 + 0.5),
            };
            ui.mascot.set_level(*level);
            changed = true;
        }
        if changed {
            self.mark_dirty();
        }
        changed
    }

    fn autoscroll_bottom(&mut self, key: u64) {
        let h = self.content_h_of(key);
        self.set_scroll(key, (h).max(0.0));
        // Content height is only known after the next render; finish the scroll there.
        self.pin_bottom = Some(key);
    }

    /// Handles one input event.
    pub fn handle(&mut self, ev: InputEvent, ui: &mut Ui) {
        self.mark_dirty();
        // Input-test mode intercepts everything.
        if self.active() == Screen::InputTest {
            match ev.phase {
                Phase::Press => {
                    if !self.held.contains(&ev.button) {
                        self.held.push(ev.button);
                    }
                    self.last_pressed = Some(ev.button);
                    if ev.button == Button::B && self.held.len() == 1 {
                        // allow leaving with a long-press handled elsewhere; tap B exits test
                    }
                }
                Phase::Release { .. } => self.held.retain(|b| *b != ev.button),
                _ => {}
            }
            if ev.just_pressed(Button::Menu) {
                self.pop();
            }
            return;
        }

        // Overlays consume input first.
        if !matches!(self.overlay, Overlay::None) {
            self.handle_overlay(ev, ui);
            return;
        }

        // During a call: F1 mutes (there is no voice note mid-call) and B interrupts Muse
        // while it speaks. Everything else works as usual, so the chat can still be read.
        if let Some(call) = self.data.call() {
            if ev.button == Button::F1 {
                if ev.just_pressed(Button::F1) {
                    self.data.call_mute(!call.muted);
                }
                return;
            }
            if ev.just_pressed(Button::B) && call.phase == crate::data::CallPhase::Speaking {
                self.data.call_barge_in();
                return;
            }
        }

        // Global talk/call bindings.
        if ev.just_pressed(Button::F1) {
            // Record from the press, so the first words are kept; a tap discards it.
            if let Err(e) = self.data.ptt_start() {
                self.show_toast(ToastKind::Error, "Microphone", &e);
            }
            return;
        }
        if ev.held(Button::F1) {
            self.overlay = Overlay::Voice {
                dictation: false,
                level: 0.0,
                start_ms: self.now_ms,
            };
            ui.mascot.set_state(goblin_ui_kit::MascotState::Listening);
            return;
        }
        if ev.released(Button::F1) {
            let _ = self.data.ptt_stop(true);
            // A tap (the hold would have opened the recorder). There is no dictation: the
            // device token has no speech-to-text route, so voice goes as a note.
            self.show_toast(
                ToastKind::Info,
                "Hold F1 to talk",
                "Release to send, B to cancel",
            );
            return;
        }
        if ev.just_pressed(Button::F2) {
            if self.data.call().is_some() {
                self.data.call_end();
            } else {
                match self.data.call_start() {
                    // The call lives in the chat, as in the Muse app.
                    Ok(()) => self.show_chat_tab(),
                    Err(e) => self.show_toast(ToastKind::Error, "No call", &e),
                }
            }
            return;
        }

        // Keyboard mode.
        if self.keyboard_open {
            self.handle_keyboard(ev, ui);
            return;
        }

        // B skips a reply being spoken (before it means "back").
        if ev.just_pressed(Button::B) && self.data.speaking() {
            self.data.stop_speaking();
            self.show_toast(ToastKind::Info, "Skipped", "");
            return;
        }

        // Navigation stack / tabs.
        if ev.just_pressed(Button::Menu) {
            self.open_panel(false);
            return;
        }
        if ev.just_pressed(Button::Select) {
            if self.active() == Screen::Notifications {
                self.push(Screen::Status);
            } else {
                self.push(Screen::Notifications);
            }
            return;
        }
        if ev.just_pressed(Button::B) {
            if self.stack.is_empty() && self.chat_scrolled_back() {
                // Reading back: B returns to the newest message first.
                let key = Screen::Chat.key();
                let max = self.content_h_of(key).max(0.0);
                self.set_scroll(key, max);
                self.pin_bottom = Some(key);
                self.mark_dirty();
            } else if !self.stack.is_empty() {
                self.pop();
            } else if self.active() == Screen::Chat && self.data.active_chat() != MAIN_CHAT {
                // B in a side chat goes back to the main chat.
                self.open_chat(MAIN_CHAT);
            }
            return;
        }
        // Tab switching only at a tab screen.
        if self.stack.is_empty() {
            if ev.pressed(Button::L1) {
                self.tab = self.tab.saturating_sub(1);
                return;
            }
            if ev.pressed(Button::R1) {
                self.tab = (self.tab + 1).min(self.tabs().len() - 1);
                return;
            }
        }

        // D-pad: move focus; when there is nothing to focus that way, scroll instead (reading
        // back through a conversation, or a screen with no focusable rows).
        for dir in [Button::Up, Button::Down, Button::Left, Button::Right] {
            if ev.pressed(dir) {
                // In Settings, left and right change the focused row's value.
                if self.active() == Screen::Settings && matches!(dir, Button::Left | Button::Right)
                {
                    if let Some(id) = ui.focus.current() {
                        self.adjust_setting(id, if dir == Button::Left { -1 } else { 1 });
                    }
                    return;
                }
                let moved = ui.focus.move_focus(dir);
                if !moved && matches!(dir, Button::Up | Button::Down) {
                    let step = self.theme.metrics.row * 2.0;
                    self.scroll_by(if dir == Button::Up { -step } else { step });
                }
                return;
            }
        }
        // Page up/down.
        if ev.pressed(Button::L2) {
            self.page(ui, -1);
            return;
        }
        if ev.pressed(Button::R2) {
            self.page(ui, 1);
            return;
        }

        if ev.just_pressed(Button::Y) && self.active() == Screen::Notifications {
            self.data.mark_notifications_seen();
            self.show_toast(ToastKind::Info, "All marked read", "");
            return;
        }
        // Direct access: Y on Chat opens the side chats; X on that list starts a new one.
        if ev.just_pressed(Button::Y) && self.active().is_tab() {
            self.open_panel(true);
            return;
        }
        if ev.just_pressed(Button::X) && self.active() == Screen::SideChats {
            if let Err(e) = self.data.new_side_chat() {
                self.show_toast(ToastKind::Error, "No side chat", &e);
            } else {
                self.show_chat_tab();
                self.show_toast(
                    ToastKind::Info,
                    "New side chat",
                    "Say something to start it",
                );
            }
            return;
        }
        // Composer / quick replies.
        if ev.just_pressed(Button::X) && self.active() == Screen::Chat {
            self.keyboard_open = true;
            self.kb = KeyboardState::default();
            return;
        }
        // Send.
        if ev.just_pressed(Button::Start)
            && self.active() == Screen::Chat
            && !self.composer.is_empty()
        {
            self.send_composer(ui);
            return;
        }

        // Activate focused item.
        if ev.just_pressed(Button::A) {
            self.activate(ui.focus.current(), ui);
        }
    }

    /// L2 / R2: most of a screenful, so a line of context stays.
    fn page(&mut self, ui: &mut Ui, dir: i32) {
        let _ = ui;
        let step = (self.body_h * 0.8).max(self.theme.metrics.row * 4.0);
        self.scroll_by(step * dir as f32);
    }

    /// Stops the clip in the viewer and goes back to its poster.
    fn stop_clip(&mut self) {
        self.player = None;
        self.player_frame = None;
        self.clip_paused = None;
        self.clip_pos = 0.0;
        self.clip_len = None;
        self.clip_secs = None;
    }

    fn scroll_by(&mut self, delta: f32) {
        let key = self.active().key();
        let max = self.content_h_of(key).max(0.0);
        let next = (self.scroll_of(key) + delta).clamp(0.0, max);
        self.set_scroll(key, next);
        // The owner is scrolling: stop bringing the focused item into view.
        self.focus_settling = false;
        // Scrolling up releases the pin; reaching the end again restores it.
        self.pin_bottom = (next >= max).then_some(key);
        self.mark_dirty();
    }

    fn handle_overlay(&mut self, ev: InputEvent, ui: &mut Ui) {
        match self.overlay {
            Overlay::Voice {
                dictation,
                start_ms,
                ..
            } => {
                let is_note = !dictation;
                if is_note && ev.released(Button::F1) {
                    let dur = self.now_ms.saturating_sub(start_ms);
                    self.overlay = Overlay::None;
                    ui.mascot.set_state(goblin_ui_kit::MascotState::Idle);
                    // The take started at the press; the hold (450 ms) has passed, so send it
                    // unless it was released at once.
                    let cancel = dur < 100;
                    match self.data.ptt_stop(cancel) {
                        Ok(()) if !cancel => {
                            ui.mascot.set_state(goblin_ui_kit::MascotState::Thinking);
                            self.show_toast(ToastKind::Success, "Voice note sent", "");
                        }
                        Ok(()) => {}
                        Err(e) => self.show_toast(ToastKind::Error, "Not sent", &e),
                    }
                    return;
                }
                if ev.just_pressed(Button::B) {
                    if is_note {
                        let _ = self.data.ptt_stop(true);
                    }
                    self.overlay = Overlay::None;
                    ui.mascot.set_state(goblin_ui_kit::MascotState::Idle);
                    return;
                }
                if dictation && ev.just_pressed(Button::A) {
                    self.composer.push_str("dictated text ");
                    self.overlay = Overlay::None;
                    ui.mascot.set_state(goblin_ui_kit::MascotState::Idle);
                }
            }
            Overlay::Confirm {
                focus,
                danger,
                title,
                message,
                confirm,
                then,
            } => {
                let mut focus = focus;
                if ev.pressed(Button::Left) {
                    focus = 0;
                }
                if ev.pressed(Button::Right) {
                    focus = 1;
                }
                if ev.just_pressed(Button::B) {
                    self.overlay = Overlay::None;
                    return;
                }
                if ev.just_pressed(Button::A) {
                    self.overlay = Overlay::None;
                    if focus == 1 {
                        match then {
                            ConfirmAction::Quit => self.quit = true,
                            ConfirmAction::ForgetChat(i) => {
                                let id = self.data.side_chats().get(i).map(|c| c.id.clone());
                                if let Some(id) = id {
                                    if self.data.active_chat() == id {
                                        self.open_chat(MAIN_CHAT);
                                    }
                                    match self.data.forget_side_chat(&id) {
                                        Ok(()) => {
                                            self.show_toast(ToastKind::Info, "Chat forgotten", "")
                                        }
                                        Err(e) => {
                                            self.show_toast(ToastKind::Error, "Not forgotten", &e)
                                        }
                                    }
                                }
                            }
                            ConfirmAction::Nothing => {}
                        }
                    }
                    return;
                }
                self.overlay = Overlay::Confirm {
                    focus,
                    danger,
                    title,
                    message,
                    confirm,
                    then,
                };
            }
            Overlay::Profile { focus, armed } => self.handle_profile_sheet(ev, focus, armed),
            Overlay::Pairing => {
                let ended = self.data.pairing().map_or(true, |p| p.step.ended());
                if ev.just_pressed(Button::B) || (ended && ev.just_pressed(Button::A)) {
                    if !ended {
                        self.data.pair_cancel();
                    }
                    self.data.pairing_seen();
                    self.overlay = Overlay::None;
                }
            }
            Overlay::Viewer { index } => {
                let n = self.viewer_items.len().max(1);
                let step = if ev.pressed(Button::Left) {
                    Some(n - 1)
                } else if ev.pressed(Button::Right) {
                    Some(1)
                } else {
                    None
                };
                if let Some(step) = step {
                    self.stop_clip();
                    self.overlay = Overlay::Viewer {
                        index: (index + step) % n,
                    };
                    return;
                }
                if ev.just_pressed(Button::B) {
                    // B stops a playing or paused clip first, then closes.
                    if self.player.is_some() || self.clip_paused.is_some() {
                        self.stop_clip();
                    } else {
                        self.overlay = Overlay::None;
                    }
                    return;
                }
                if ev.just_pressed(Button::A) {
                    let clip = self.viewer_items.get(index).and_then(|m| m.video.clone());
                    if let Some(player) = self.player.take() {
                        // Pause: the frame on screen stays, and A goes on from here.
                        self.clip_paused = Some(player.position());
                    } else if let Some(path) = clip {
                        let from = self.clip_paused.take().unwrap_or(0.0);
                        if from == 0.0 {
                            // The clip's length, for the progress line (read off this thread).
                            let (tx, rx) = std::sync::mpsc::channel();
                            let probe = path.clone();
                            std::thread::spawn(move || {
                                let _ = tx.send(crate::media::clip_seconds(&probe));
                            });
                            self.clip_len = Some(rx);
                            self.clip_secs = None;
                        }
                        self.clip_pos = from;
                        match crate::media::ClipPlayer::start(&path, from) {
                            Ok(p) => self.player = Some(p),
                            Err(e) => self.show_toast(ToastKind::Error, "Can't play", &e),
                        }
                    }
                }
            }
            Overlay::Panel { focus } => {
                if self.panel_closing {
                    return; // already on its way out
                }
                let items = self.panel_items();
                let len = items.len().max(1);
                let mut focus = focus.min(len - 1);
                // Up and down wrap, so the far end is one press away.
                if ev.pressed(Button::Up) {
                    focus = (focus + len - 1) % len;
                }
                if ev.pressed(Button::Down) {
                    focus = (focus + 1) % len;
                }
                if ev.just_pressed(Button::Y) {
                    // Y on a side chat forgets it (after a confirmation); elsewhere it closes.
                    if let Some(PanelItem::Side(i)) = items.get(focus) {
                        self.panel_anim.snap(0.0);
                        self.overlay = Overlay::Confirm {
                            title: "Forget this chat?",
                            message:
                                "It is removed from this device only. Muse keeps the conversation.",
                            confirm: "Forget",
                            danger: true,
                            focus: 0,
                            then: ConfirmAction::ForgetChat(*i),
                        };
                        return;
                    }
                    self.close_panel(None);
                    return;
                }
                if ev.just_pressed(Button::Select) {
                    // Select on a side chat renames it: the chat opens with the keyboard
                    // holding its title.
                    if let Some(PanelItem::Side(i)) = items.get(focus) {
                        if let Some(c) = self.data.side_chats().get(*i) {
                            let (id, title) = (c.id.clone(), c.title.clone());
                            self.panel_anim.snap(0.0);
                            self.overlay = Overlay::None;
                            self.open_chat(&id);
                            let draft = std::mem::replace(&mut self.composer, title);
                            self.renaming = Some((RenameTarget::Chat(id), draft));
                            self.keyboard_open = true;
                            self.mark_dirty();
                        }
                    }
                    return;
                }
                if ev.just_pressed(Button::B) || ev.just_pressed(Button::Menu) {
                    self.close_panel(None);
                    return;
                }
                if ev.just_pressed(Button::X) {
                    self.close_panel(Some(PanelItem::NewChat));
                    return;
                }
                if ev.just_pressed(Button::A) {
                    if let Some(item) = items.get(focus) {
                        self.close_panel(Some(*item));
                    }
                    return;
                }
                self.overlay = Overlay::Panel { focus };
            }
            Overlay::None => {}
        }
    }

    /// The panel's rows, top to bottom: tabs, side chats, then the device entries.
    fn panel_items(&self) -> Vec<PanelItem> {
        let mut v: Vec<PanelItem> = (0..self.tabs().len()).map(PanelItem::Tab).collect();
        v.push(PanelItem::NewChat);
        v.extend((0..self.data.side_chats().len()).map(PanelItem::Side));
        v.extend([
            PanelItem::Inbox,
            PanelItem::Status,
            PanelItem::Settings,
            PanelItem::InputTest,
            PanelItem::Quit,
        ]);
        v
    }

    /// Slides the panel in, focused on where the owner is now (or on the side chats).
    fn open_panel(&mut self, at_side_chats: bool) {
        let items = self.panel_items();
        let active_chat = self.data.active_chat().to_owned();
        let on_side = self
            .data
            .side_chats()
            .iter()
            .position(|c| c.id == active_chat);
        let want = match (at_side_chats, on_side) {
            (_, Some(i)) if self.active() == Screen::Chat => PanelItem::Side(i),
            (true, _) => PanelItem::NewChat,
            _ => PanelItem::Tab(self.tab),
        };
        let focus = items.iter().position(|i| *i == want).unwrap_or(0);
        self.overlay = Overlay::Panel { focus };
        self.panel_closing = false;
        self.panel_then = None;
        if self.reduce_motion {
            self.panel_anim.snap(1.0);
        } else {
            self.panel_anim.set(1.0, PANEL_IN_MS, self.now_ms);
        }
        self.mark_dirty();
    }

    /// Slides the panel out, then runs `then`.
    fn close_panel(&mut self, then: Option<PanelItem>) {
        self.panel_then = then;
        self.panel_closing = true;
        if self.reduce_motion {
            self.panel_anim.snap(0.0);
        } else {
            self.panel_anim.set(0.0, PANEL_OUT_MS, self.now_ms);
        }
        self.mark_dirty();
    }

    fn run_panel(&mut self, item: PanelItem) {
        match item {
            PanelItem::Tab(i) => {
                while !self.stack.is_empty() {
                    self.pop();
                }
                self.tab = i.min(self.tabs().len() - 1);
                if self.active() == Screen::Chat {
                    self.open_chat(MAIN_CHAT);
                }
            }
            PanelItem::NewChat => {
                if let Err(e) = self.data.new_side_chat() {
                    self.show_toast(ToastKind::Error, "No side chat", &e);
                } else {
                    self.show_chat_tab();
                    self.show_toast(
                        ToastKind::Info,
                        "New side chat",
                        "Say something to start it",
                    );
                }
            }
            PanelItem::Side(i) => {
                if let Some(id) = self.data.side_chats().get(i).map(|c| c.id.clone()) {
                    self.open_chat(&id);
                }
            }
            PanelItem::Inbox => self.push(Screen::Notifications),
            PanelItem::Status => self.push(Screen::Status),
            PanelItem::Settings => self.push(Screen::Settings),
            PanelItem::InputTest => self.push(Screen::InputTest),
            PanelItem::Quit => {
                self.overlay = Overlay::Confirm {
                    title: "Quit to launcher?",
                    message:
                        "Muse keeps running in the background. You can reopen from the Tools list.",
                    confirm: "Quit",
                    danger: true,
                    focus: 0,
                    then: ConfirmAction::Quit,
                };
            }
        }
        self.mark_dirty();
    }

    /// Keyboard: A types the focused key; the rest are direct buttons, so common actions never
    /// need a trip across the grid. X backspace · Y space · L1 shift · R1 symbols · Start send
    /// · B close (the draft is kept).
    fn handle_keyboard(&mut self, ev: InputEvent, ui: &mut Ui) {
        use goblin_ui_kit::widgets::KeyAction;
        if ev.just_pressed(Button::B) {
            self.keyboard_open = false;
            if let Some((_, draft)) = self.renaming.take() {
                self.composer = draft; // renaming cancelled: the message draft comes back
            }
            return;
        }
        if ev.just_pressed(Button::Start) {
            if let Some((target, draft)) = self.renaming.clone() {
                let renamed = match &target {
                    RenameTarget::Chat(id) => {
                        // The new title (1 to 60 characters); an empty one keeps the
                        // keyboard up.
                        let title: String = self.composer.trim().chars().take(60).collect();
                        if title.is_empty() {
                            return;
                        }
                        self.data.rename_side_chat(id, &title)
                    }
                    RenameTarget::Profile(name) => match profile_name_from(&self.composer) {
                        Ok(new_name) if new_name == *name => Ok(()),
                        Ok(new_name) => self.data.rename_profile(name, &new_name).map(|()| {
                            self.show_toast(ToastKind::Info, "Profile renamed", &new_name);
                        }),
                        Err(rule) => Err(rule.to_owned()),
                    },
                };
                match renamed {
                    Ok(()) => {
                        self.renaming = None;
                        self.composer = draft;
                        self.keyboard_open = false;
                        if matches!(target, RenameTarget::Profile(_)) {
                            // Back to the profiles, where the new name shows.
                            self.push(Screen::Settings);
                        }
                    }
                    Err(e) => self.show_toast(ToastKind::Error, "Not renamed", &e),
                }
                return;
            }
            self.keyboard_open = false;
            self.send_composer(ui);
            return;
        }
        if ev.pressed(Button::X) {
            self.composer.pop();
            return;
        }
        if ev.just_pressed(Button::Y) {
            self.composer.push(' ');
            return;
        }
        if ev.just_pressed(Button::L1) {
            self.kb.toggle_case();
            return;
        }
        if ev.just_pressed(Button::R1) {
            self.kb.toggle_symbols();
            return;
        }
        for d in [Button::Up, Button::Down, Button::Left, Button::Right] {
            if ev.pressed(d) {
                self.kb.navigate(d);
                return;
            }
        }
        if ev.just_pressed(Button::A) {
            match self.kb.action() {
                KeyAction::Char(c) => {
                    self.composer.push(c);
                    // One capital, then back to lower case (as phone keyboards do).
                    if self.kb.layer == goblin_ui_kit::widgets::KeyboardLayer::Upper {
                        self.kb.toggle_case();
                    }
                }
                KeyAction::Space => self.composer.push(' '),
                _ => {}
            }
        }
    }

    fn send_composer(&mut self, ui: &mut Ui) {
        if self.composer.trim().is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.composer);
        if !self.send_text(&text, ui) {
            self.composer = text; // keep what the user typed so they can retry
        }
    }

    /// Sends `text` to the main chat; returns false (after a toast) when it could not be sent.
    fn send_text(&mut self, text: &str, ui: &mut Ui) -> bool {
        match self.data.send_user(text) {
            Ok(()) => {
                ui.mascot.set_state(goblin_ui_kit::MascotState::Thinking);
                self.autoscroll_bottom(Screen::Chat.key());
                true
            }
            Err(e) => {
                self.show_toast(ToastKind::Error, "Not sent", &e);
                false
            }
        }
    }

    fn activate(&mut self, focused: Option<FocusId>, ui: &mut Ui) {
        let screen = self.active();
        match screen {
            Screen::Chat => {
                // Focused quick-reply chip (ids 100+) sends it.
                if let Some(id) = focused {
                    if id >= screens::MEDIA_FOCUS_BASE {
                        // A picture or clip: open it full screen.
                        self.viewer_items = self
                            .data
                            .main_chat()
                            .iter()
                            .flat_map(|t| t.media.iter().cloned())
                            .collect();
                        let index = (id - screens::MEDIA_FOCUS_BASE) as usize;
                        if index < self.viewer_items.len() {
                            self.overlay = Overlay::Viewer { index };
                        }
                        return;
                    }
                    let pills = self.data.quick_replies();
                    if (100..100 + pills.len() as u64).contains(&id) {
                        let text = pills[(id - 100) as usize].clone();
                        self.send_text(&text, ui);
                    } else if id == 1 {
                        self.keyboard_open = true;
                    }
                }
            }
            Screen::Settings => self.activate_settings(focused, ui),
            Screen::Appearance => self.activate_appearance(focused),
            Screen::SideChats => match focused {
                Some(screens::NEW_SIDE_CHAT) => {
                    if let Err(e) = self.data.new_side_chat() {
                        self.show_toast(ToastKind::Error, "No side chat", &e);
                    } else {
                        // The provider opens the new chat once the daemon has created it.
                        self.show_chat_tab();
                        self.show_toast(
                            ToastKind::Info,
                            "New side chat",
                            "Say something to start it",
                        );
                    }
                }
                Some(i) => {
                    if let Some(id) = self.data.side_chats().get(i as usize).map(|c| c.id.clone()) {
                        self.open_chat(&id);
                    }
                }
                None => {}
            },
            Screen::Notifications => {
                // A notice about a chat opens it (leaving the inbox marks it seen).
                let chat = focused
                    .and_then(|i| self.data.notifications().get(i as usize))
                    .and_then(|n| n.chat.clone());
                if let Some(chat) = chat {
                    self.open_chat(&chat);
                }
            }
            _ => {}
        }
    }

    /// The profile sheet: Up/Down choose, A does it (twice for what cannot be undone here).
    fn handle_profile_sheet(&mut self, ev: InputEvent, focus: usize, armed: Option<usize>) {
        let Some(entry) = self
            .data
            .profiles()
            .into_iter()
            .find(|p| p.name == self.sheet_profile)
        else {
            self.overlay = Overlay::None;
            return;
        };
        let actions = profile_actions(&entry);
        if ev.just_pressed(Button::B) {
            self.overlay = Overlay::None;
            return;
        }
        let mut focus = focus.min(actions.len().saturating_sub(1));
        if ev.pressed(Button::Up) && focus > 0 {
            focus -= 1;
        }
        if ev.pressed(Button::Down) && focus + 1 < actions.len() {
            focus += 1;
        }
        if !ev.just_pressed(Button::A) {
            // Moving away disarms.
            let armed = armed.filter(|a| *a == focus);
            self.overlay = Overlay::Profile { focus, armed };
            return;
        }
        let name = entry.name.clone();
        let (action, _) = actions[focus];
        let needs_second_press = matches!(action, ProfileAction::Unpair | ProfileAction::Delete);
        if needs_second_press && armed != Some(focus) {
            self.overlay = Overlay::Profile {
                focus,
                armed: Some(focus),
            };
            return;
        }
        self.overlay = Overlay::None;
        let result = match action {
            ProfileAction::Switch => {
                // Whatever is open belongs to the profile being left.
                self.keyboard_open = false;
                self.composer.clear();
                self.data.switch_profile(&name).map(|()| {
                    self.show_toast(ToastKind::Info, "Switching profile", &name);
                    self.open_chat(MAIN_CHAT);
                })
            }
            ProfileAction::Rename => {
                // The keyboard lives with the chat: it opens there holding the name, and
                // Start saves it. The message being written comes back afterwards.
                self.open_chat(MAIN_CHAT);
                let draft = std::mem::replace(&mut self.composer, name.clone());
                self.renaming = Some((RenameTarget::Profile(name.clone()), draft));
                self.keyboard_open = true;
                self.show_toast(
                    ToastKind::Info,
                    "Rename profile",
                    "Type the new name, then press Start",
                );
                Ok(())
            }
            ProfileAction::Pair => self.data.pair_start(&name).map(|()| {
                self.overlay = Overlay::Pairing;
            }),
            ProfileAction::Unpair => self
                .data
                .unpair_profile(&name)
                .map(|()| self.show_toast(ToastKind::Info, "Unpaired", &name)),
            ProfileAction::Delete => self
                .data
                .delete_profile(&name)
                .map(|()| self.show_toast(ToastKind::Info, "Profile deleted", &name)),
        };
        if let Err(e) = result {
            self.show_toast(ToastKind::Error, "Not done", &e);
        }
    }

    fn activate_settings(&mut self, focused: Option<FocusId>, _ui: &mut Ui) {
        use screens::setting_id as sid;
        if focused == Some(sid::NEW_PROFILE) {
            match self.data.create_profile() {
                Ok(name) => {
                    self.sheet_profile = name;
                    self.overlay = Overlay::Profile {
                        focus: 0,
                        armed: None,
                    };
                }
                Err(e) => self.show_toast(ToastKind::Error, "No new profile", &e),
            }
            self.mark_dirty();
            return;
        }
        if let Some(i) = focused.filter(|id| *id >= sid::PROFILE_BASE) {
            if let Some(p) = self.data.profiles().get((i - sid::PROFILE_BASE) as usize) {
                self.sheet_profile = p.name.clone();
                self.overlay = Overlay::Profile {
                    focus: 0,
                    armed: None,
                };
                self.mark_dirty();
            }
            return;
        }
        if focused == Some(screens::setting_id::CHANGE_AVATAR) {
            // Muse makes its own avatar when asked. Open the main chat with the request
            // started; the owner finishes it and sends it, so nothing changes unasked.
            self.open_chat(MAIN_CHAT);
            if self.composer.trim().is_empty() {
                self.composer = "Change your avatar to ".into();
            }
            self.keyboard_open = true;
            self.mark_dirty();
            return;
        }
        if let Some(id) = focused {
            self.adjust_setting(id, 1);
        }
    }

    /// Changes the focused setting: a switch flips, a choice steps by `dir` (wrapping). The
    /// change goes to the daemon, which saves it; the app re-applies settings when they change.
    fn adjust_setting(&mut self, id: FocusId, dir: i32) {
        use screens::setting_id as sid;
        let step = |len: usize, cur: usize| ((cur as i32 + dir).rem_euclid(len as i32)) as usize;
        let result = match id {
            sid::SIZE => {
                let all = goblin_ui_kit::UiSize::ALL;
                let cur = all
                    .iter()
                    .position(|s| *s == self.theme.ui_size)
                    .unwrap_or(1);
                let next = all[step(all.len(), cur)];
                self.data
                    .set_setting("appearance.ui_size", next.id().into())
            }
            sid::MODE => {
                let next = if self.theme.mode == Mode::Dark {
                    "light"
                } else {
                    "dark"
                };
                self.data.set_setting("appearance.mode", next.into())
            }
            sid::CHAT_THEME => {
                let all = goblin_ui_kit::theme::CHAT_THEMES;
                let cur = all
                    .iter()
                    .position(|t| t.id == self.theme.chat_theme.id)
                    .unwrap_or(0);
                let next = all[step(all.len(), cur)].id;
                self.data.set_setting("appearance.chat_theme", next.into())
            }
            sid::AVATAR => {
                let all = AvatarSize::ALL;
                let cur = all
                    .iter()
                    .position(|a| *a == self.theme.avatar)
                    .unwrap_or(2);
                let next = avatar_id(all[step(all.len(), cur)]);
                self.data.set_setting("appearance.avatar_size", next.into())
            }
            sid::REDUCE_MOTION => self
                .data
                .set_setting("appearance.reduce_motion", (!self.reduce_motion).into()),
            sid::VOICE_TO_VOICE => {
                let on = !self.data.voice_to_voice();
                self.data.set_voice_to_voice(on)
            }
            sid::CALL_GREETING => {
                let on = !self.setting_bool("voice.call_greeting", false);
                self.data.set_setting("voice.call_greeting", on.into())
            }
            sid::LIGHTS => {
                let on = self.lights_on();
                self.data
                    .set_setting("leds.mode", (if on { "off" } else { "presence" }).into())
            }
            sid::VIBRATE => {
                let on = !self.setting_bool("haptics.enabled", true);
                self.data.set_setting("haptics.enabled", on.into())
            }
            sid::KEEP_CONNECTED => {
                let on = !self.setting_bool("background.keep_connected", true);
                self.data
                    .set_setting("background.keep_connected", on.into())
            }
            sid::AUTOSTART => {
                let on = !self.data.autostart().unwrap_or(false);
                self.data.set_autostart(on)
            }
            sid::FILES => {
                let all = goblin_ipc::types::FileScope::ALL;
                let cur = all
                    .iter()
                    .position(|s| *s == self.data.consent().files)
                    .unwrap_or(0);
                self.data.set_file_scope(all[step(all.len(), cur)])
            }
            // Profile rows open a sheet with A; Left and Right do nothing on them.
            id if id >= sid::PROFILE_BASE || id == sid::NEW_PROFILE => Ok(()),
            id if sid::CONSENT.contains(&id) => {
                let kind = match sid::CONSENT.iter().position(|c| *c == id) {
                    Some(0) => ConsentKind::Shell,
                    Some(1) => ConsentKind::FileRead,
                    Some(2) => ConsentKind::FileWrite,
                    _ => ConsentKind::DropIns,
                };
                let on = !kind.get(&self.data.consent());
                self.data.set_consent(kind, on)
            }
            _ => Ok(()),
        };
        if let Err(e) = result {
            self.show_toast(ToastKind::Error, "Not changed", &e);
        }
        self.apply_settings();
        self.mark_dirty();
    }

    fn setting_bool(&self, key: &str, default: bool) -> bool {
        self.data
            .setting(key)
            .and_then(|v| v.as_bool())
            .unwrap_or(default)
    }

    fn lights_on(&self) -> bool {
        self.data
            .setting("leds.mode")
            .and_then(|v| v.as_str().map(|s| s != "off"))
            .unwrap_or(true)
    }

    /// Applies the appearance settings the provider holds to the theme. A setting the provider
    /// does not know leaves the current value alone.
    fn apply_settings(&mut self) {
        self.settings_seen = self.data.settings_rev();
        let get = |k: &str| self.data.setting(k);
        let text = |k: &str| get(k).and_then(|v| v.as_str().map(str::to_owned));
        if let Some(size) =
            text("appearance.ui_size").and_then(|s| goblin_ui_kit::UiSize::from_id(&s))
        {
            if size != self.theme.ui_size {
                self.theme.set_ui_size(size);
            }
        }
        match text("appearance.mode").as_deref() {
            Some("dark") if self.theme.mode != Mode::Dark => self.theme.set_mode(Mode::Dark),
            Some("light") if self.theme.mode != Mode::Light => self.theme.set_mode(Mode::Light),
            _ => {} // "system": keep what the app started with
        }
        if let Some(ct) = text("appearance.chat_theme") {
            if ct != self.theme.chat_theme.id {
                self.theme.set_chat_theme(&ct);
            }
        }
        if let Some(a) = text("appearance.avatar_size") {
            if let Some(size) = AvatarSize::ALL.iter().find(|s| avatar_id(**s) == a) {
                self.theme.avatar = *size;
            }
        }
        if let Some(rm) = get("appearance.reduce_motion").and_then(|v| v.as_bool()) {
            self.reduce_motion = rm;
            self.theme.reduce_motion = rm;
        }
    }

    fn activate_appearance(&mut self, focused: Option<FocusId>) {
        match focused {
            // Mode choices 20/21/22
            Some(20) => self.theme.set_mode(Mode::Light),
            Some(21) => self.theme.set_mode(Mode::Dark),
            // avatar sizes 30..35
            Some(id) if (30..30 + AvatarSize::ALL.len() as u64).contains(&id) => {
                self.theme.avatar = AvatarSize::ALL[(id - 30) as usize];
            }
            // chat themes 40..
            Some(id) if (40..40 + goblin_ui_kit::theme::CHAT_THEMES.len() as u64).contains(&id) => {
                let ct = goblin_ui_kit::theme::CHAT_THEMES[(id - 40) as usize];
                self.theme.set_chat_theme(ct.id);
            }
            _ => {}
        }
    }

    /// Switches to the input-test screen (for `--input-test`).
    pub fn enter_input_test(&mut self) {
        self.stack.clear();
        self.push(Screen::InputTest);
    }

    /// Forces a specific screen for `--screenshot`.
    pub fn force_screen(&mut self, name: &str) -> bool {
        // "screen@scenario" applies a demo scenario first (mock data only).
        if let Some((screen, scenario)) = name.split_once('@') {
            self.data.set_demo_scenario(scenario);
            if scenario == "long" {
                // Open the first side chat so its long title shows in the header.
                if screen == "chat" {
                    return self.force_screen("sidechat");
                }
            }
            return self.force_screen(screen);
        }
        self.stack.clear();
        let tabs = self.tabs();
        let set_tab = |app: &mut Self, s: Screen| {
            if let Some(i) = app.tabs().iter().position(|(sc, _)| *sc == s) {
                app.tab = i;
            }
        };
        match name {
            "chat" => {
                set_tab(self, Screen::Chat);
                // As on the device: a chat opens at its newest message.
                self.autoscroll_bottom(Screen::Chat.key());
            }
            "sidechats" => self.push(Screen::SideChats),
            // The profiles in Settings (scrolled to them), a profile's sheet, and pairing.
            "profiles" | "profile-sheet" | "pairing" => {
                self.push(Screen::Settings);
                self.focus_request = Some(screens::setting_id::PROFILE_BASE + 1);
                if name == "profile-sheet" {
                    self.sheet_profile = "work".into();
                    self.overlay = Overlay::Profile {
                        focus: 0,
                        armed: None,
                    };
                }
                if name == "pairing" {
                    let _ = self.data.pair_start("work");
                    self.overlay = Overlay::Pairing;
                }
            }
            "pills" | "pills-long" => {
                set_tab(self, Screen::Chat);
                // Worst cases for the 60-character budget: many short, or a few long.
                let demo: &[&str] = if name == "pills" {
                    &[
                        "Yes please",
                        "No thanks",
                        "Maybe later",
                        "Tell me more",
                        "Why not?",
                        "Sure, go on",
                    ]
                } else {
                    &[
                        "I'd like the first one",
                        "Show me the others too",
                        "Not today",
                    ]
                };
                self.data
                    .set_demo_quick_replies(demo.iter().map(|s| s.to_string()).collect());
            }
            "menu" | "panel" => {
                set_tab(self, Screen::Chat);
                self.autoscroll_bottom(Screen::Chat.key());
                self.open_panel(name == "panel");
                self.panel_anim.snap(1.0);
            }
            "volume" => {
                set_tab(self, Screen::Chat);
                self.sysvol = Some(goblin_platform::sysvol::SystemVolume {
                    percent: 40,
                    muted: false,
                    headphones: false,
                });
                self.sysvol_hud_until_ms = u64::MAX;
            }
            "sidechat" => {
                if let Some(id) = self.data.side_chats().first().map(|c| c.id.clone()) {
                    self.open_chat(&id);
                }
            }
            "notifications" => self.push(Screen::Notifications),
            "status" => self.push(Screen::Status),
            "settings" => self.push(Screen::Settings),
            "appearance" => self.push(Screen::Appearance),
            "call" => {
                set_tab(self, Screen::Chat);
                self.autoscroll_bottom(Screen::Chat.key());
                let _ = self.data.call_start();
                self.call_avatar.snap(1.0);
            }
            "viewer" => {
                set_tab(self, Screen::Chat);
                self.viewer_items = self
                    .data
                    .main_chat()
                    .iter()
                    .flat_map(|t| t.media.iter().cloned())
                    .collect();
                self.overlay = Overlay::Viewer { index: 0 };
            }
            "recording" => {
                set_tab(self, Screen::Chat);
                self.autoscroll_bottom(Screen::Chat.key());
                self.overlay = Overlay::Voice {
                    dictation: false,
                    level: 0.6,
                    start_ms: 0,
                };
            }
            "keyboard" => {
                set_tab(self, Screen::Chat);
                self.composer = "Draft reply".into();
                self.keyboard_open = true;
                self.autoscroll_bottom(Screen::Chat.key());
            }
            "voice" => {
                self.overlay = Overlay::Voice {
                    dictation: false,
                    level: 0.6,
                    start_ms: 0,
                };
            }
            "confirm" => {
                self.overlay = Overlay::Confirm { title: "Unlink account?", message: "M3 tabs will show cached content until you re-link. Chat and voice keep working.", confirm: "Unlink", danger: true, focus: 0, then: ConfirmAction::Nothing };
            }
            "toast" => {
                self.show_toast(ToastKind::Success, "Profile renamed", "work");
                set_tab(self, Screen::Chat);
            }
            "inputtest" => self.push(Screen::InputTest),
            "feed" | "ideas" | "goals" | "library" => {
                self.data.set_linked_demo(true);
                let s = match name {
                    "feed" => Screen::Feed,
                    "ideas" => Screen::Ideas,
                    "goals" => Screen::Goals,
                    _ => Screen::Library,
                };
                set_tab(self, s);
            }
            _ => return false,
        }
        let _ = tabs;
        true
    }

    /// Enables account-linked mode (shows the M3 tabs) for demos/screens.
    pub fn set_account_linked(&mut self, linked: bool) {
        self.data.set_linked_demo(linked);
    }

    /// Renders the whole UI into `canvas`.
    pub fn render(&mut self, canvas: &mut Canvas, ui: &mut Ui, now_ms: u64) {
        self.now_ms = now_ms;
        self.dirty = false;
        self.theme.reduce_motion = self.reduce_motion;
        let active = self.active();
        let key = active.key();

        // Begin focus for this screen. Within the same screen, keep the live focus so D-pad
        // movement persists between frames; on entering a screen, restore its remembered focus.
        let keep = if let Some(id) = self.focus_request.take() {
            Some(id)
        } else if self.last_key == Some(key) {
            ui.focus.current()
        } else {
            // A screen seen for the first time starts at its first item (`ensure`), not at
            // whatever id the previous screen had focused.
            // Chat starts at the composer (focus id 1).
            ui.memory.get(key).or((active == Screen::Chat).then_some(1))
        };
        self.last_key = Some(key);
        ui.focus.begin(keep);
        ui.ring.begin();

        let bounds = Rect::new(0.0, 0.0, canvas.width() as f32, canvas.height() as f32);
        // Paint against a snapshot of the theme so `self` stays free for `&mut self` screen
        // dispatch while the borrow-held Painter is alive. Live re-theming takes effect next
        // frame, which is imperceptible.
        let theme = self.theme.clone();
        let prof0 = std::time::Instant::now();
        let mut p = Painter::new(canvas, ui, &theme, now_ms);
        p.clear_page();

        if active == Screen::InputTest {
            screens::input_test(&mut p, &self.held, self.last_pressed);
            self.finish_focus(ui, key);
            return;
        }

        // Chrome: no title bar. A floating header (tabs or the screen's name top-left, the avatar
        // centred on Chat, the status pill top-right) and the hint bar at the bottom.
        let m = self.theme.metrics;
        let hintbar = Rect::new(bounds.x, bounds.bottom() - m.hintbar, bounds.w, m.hintbar);
        let dev = self.data.status();
        let activity = self.data.activity();
        let call_caption = self.data.call().map(|c| match (c.muted, c.phase) {
            (_, crate::data::CallPhase::Connecting) => "calling…",
            (true, _) => "on a call · muted",
            (_, crate::data::CallPhase::Listening) => "is listening",
            (_, crate::data::CallPhase::Thinking) => "is thinking",
            (_, crate::data::CallPhase::Speaking) => "is speaking",
        });
        let state_caption: &str = match (p.ui.mascot.state(), dev.link) {
            (_, LinkState::Reconnecting) => "reconnecting…",
            (_, LinkState::NotPaired) => "not paired",
            (_, LinkState::Error) => "not connected",
            _ if call_caption.is_some() => call_caption.unwrap_or(""),
            (goblin_ui_kit::MascotState::Listening, _) => "is listening",
            (goblin_ui_kit::MascotState::Speaking, _) => "is speaking",
            (goblin_ui_kit::MascotState::Thinking, _) if !activity.is_empty() => &activity,
            (goblin_ui_kit::MascotState::Thinking, _) => "is working",
            (goblin_ui_kit::MascotState::Idle, _) => "",
        };
        let clock = if self.fixed_clock {
            "7:58".to_owned()
        } else {
            local_clock()
        };
        let tabs = self.tabs();
        let tab_items: Vec<TabItem> = tabs.iter().map(|(_, t)| *t).collect();
        let on_chat = active == Screen::Chat;
        let side_title = if on_chat {
            self.data.active_chat_title().map(str::to_owned)
        } else {
            None
        };
        let agent_name = self.data.agent_name().to_owned();
        // The avatar's current picture: a clip frame while it animates, else the still.
        let avatar_state = if self.reduce_motion {
            None
        } else {
            self.avatar_state()
        };
        let now = self.now_ms;
        // Copied out (a few hundred kB, only on frames that redraw) so the rest of the frame
        // can borrow the app freely.
        let avatar_owned: Option<goblin_ui_kit::Image> = match self.avatar.as_mut() {
            Some(a) => match avatar_state {
                Some(state)
                    if a.frame_index(state, now).is_some()
                        || a.frame_index("idle", now).is_some() =>
                {
                    a.frame(state, now).cloned()
                }
                _ => a.still.clone(),
            },
            None => None,
        };
        let avatar_image = avatar_owned.as_ref();
        let header = Header {
            back_title: side_title.as_deref(),
            title: (!active.is_tab()).then(|| active.title()),
            // One tab is no choice: the strip (and its L1/R1 keys) shows only when there
            // is somewhere else to go.
            tabs: if active.is_tab() && tab_items.len() > 1 {
                &tab_items
            } else {
                &[]
            },
            selected: self.tab.min(tab_items.len().saturating_sub(1)),
            // During a call the avatar grows (shown even if the owner hides it otherwise).
            avatar_px: if on_chat {
                let base = self.theme.avatar.diameter() * self.theme.scale;
                let call = (base.max(44.0 * self.theme.scale) * 1.9).min(bounds.h * 0.3);
                let g = self.call_avatar.value();
                (base + (call - base) * g).round()
            } else {
                0.0
            },
            avatar: avatar_image,
            call: (on_chat && self.data.call().is_some()).then(|| {
                let c = self.data.call().unwrap_or(crate::data::CallView {
                    phase: crate::data::CallPhase::Connecting,
                    muted: false,
                });
                goblin_ui_kit::widgets::CallControls {
                    muted: c.muted,
                    speaking: c.phase == crate::data::CallPhase::Speaking,
                }
            }),
            name: &agent_name,
            state_caption: if on_chat { state_caption } else { "" },
            fade: on_chat,
            link: dev.link,
            battery: dev.battery,
            charging: dev.charging,
            clock: &clock,
        };
        let band = p.header_band(&header);
        let below_header = Rect::new(
            bounds.x,
            bounds.y + p.header_row(),
            bounds.w,
            bounds.h - p.header_row() - m.hintbar,
        );
        // Chat scrolls under the header (which fades it out); other screens start below it.
        let body_area = if on_chat {
            // The keyboard takes its room from the bottom, so the newest messages stay in view.
            let kb = if self.keyboard_open {
                p.keyboard_height()
            } else {
                0.0
            };
            Rect::new(bounds.x, bounds.y, bounds.w, bounds.h - m.hintbar - kb)
        } else {
            below_header
        };
        self.chat_top_inset = band;
        self.body_h = body_area.h;

        // Body (scrollable screens draw their content and report height).
        let scroll = self.scroll_shown_of(key);
        // The scroll moves by whole pixels: text and edges then look the same wherever a
        // piece has scrolled to, which is also what lets it be drawn once and moved.
        let scroll = scroll.round();
        let prof1 = std::time::Instant::now();
        let content_h = self.render_body(&mut p, active, body_area, scroll);
        let prof2 = std::time::Instant::now();
        let max_scroll = (content_h - body_area.h).max(0.0);
        if on_chat {
            if let Some(before) = self.hold_view.take() {
                let grown = max_scroll - before;
                if grown > 0.5 {
                    // Move the view by what was added above it, then draw the frame again
                    // from the start so the jump is never shown.
                    let target = self.scroll_of(key) + grown;
                    self.set_scroll(key, target);
                    for (k, v) in self.scroll_shown.iter_mut() {
                        if *k == key {
                            *v += grown;
                        }
                    }
                    self.set_content_h(key, max_scroll);
                    self.pin_bottom = (target >= max_scroll - 0.5).then_some(key);
                    return self.render(canvas, ui, now_ms);
                }
            }
            // Within half a screen of the oldest message shown: ask for older ones.
            if self.scroll_of(key) < body_area.h * 0.5
                && (self.pin_bottom != Some(key) || max_scroll < 1.0)
            {
                self.want_older = true;
            }
        }
        self.set_content_h(key, max_scroll);
        if self.pin_bottom == Some(key) && (self.scroll_of(key) - max_scroll).abs() > 0.5 {
            self.set_scroll(key, max_scroll);
            self.mark_dirty(); // draw once more at the new bottom
        }
        // Lists that continue past the edge fade out there (Chat fades its own transcript).
        if !on_chat {
            if scroll < max_scroll - 1.0 {
                p.edge_fade(body_area, true);
            }
            if scroll > 1.0 {
                p.edge_fade(body_area, false);
            }
        }
        // The header changes when what it shows does, not when the chat under it moves.
        let header_key = goblin_ui_kit::widgets::sprite_key(&(
            0x4Du8,
            (
                header.back_title,
                header.title,
                header.selected,
                header.tabs.len(),
            ),
            (
                header.avatar_px.to_bits(),
                header.name,
                header.state_caption,
            ),
            (
                header.fade,
                header.link as u8,
                header.battery,
                header.charging,
                header.clock,
            ),
            header.avatar.map(image_key),
            bounds.w.to_bits(),
        ));
        p.sprite(
            header_key,
            Rect::new(bounds.x, bounds.y, bounds.w, band + m.pad * 2.0),
            false,
            |p, r| {
                p.header(Rect::new(r.x, r.y, r.w, bounds.h), &header);
                0.0
            },
        );

        // Hint bar.
        self.settings_focus_opens = p.ui.focus.current().is_some_and(|id| {
            id >= screens::setting_id::PROFILE_BASE || id == screens::setting_id::NEW_PROFILE
        });
        let hints = self.hints_for(active);
        let hints_key = goblin_ui_kit::widgets::sprite_key(&(
            0x41u8,
            hints
                .items()
                .iter()
                .map(|h| (h.button as u8, h.label))
                .collect::<Vec<_>>(),
            hintbar.w.to_bits(),
        ));
        p.sprite(hints_key, hintbar, false, |p, r| {
            p.hint_bar(r, &hints);
            0.0
        });

        // Overlays.
        let prof3 = std::time::Instant::now();
        self.render_overlay(&mut p, bounds);

        // A reply is being spoken: say how to skip it.
        if self.data.speaking() && self.sysvol_hud_until_ms == 0 && self.toast.is_none() {
            draw_pill(&mut p, bounds, Icon::Volume, "Speaking · B to skip");
        }

        // System volume bar (volume keys).
        if self.sysvol_hud_until_ms != 0 {
            if let Some(v) = self.sysvol {
                draw_volume_hud(&mut p, bounds, v);
            }
        }

        // Toast.
        if let Some(t) = &self.toast {
            let toast = Toast {
                kind: t.kind,
                title: &t.title,
                body: &t.body,
            };
            p.toast(bounds, &toast);
        }

        // The focus ring is drawn last, gliding from the previously focused item. The viewer
        // covers the screen and has nothing to focus.
        if matches!(
            self.overlay,
            Overlay::Viewer { .. } | Overlay::Profile { .. } | Overlay::Pairing
        ) {
            p.ui.ring.target = None;
        }
        let ring_moving = p.draw_focus_ring();

        // Finish focus + scroll into view.
        self.finish_focus_scrolled(ui, key, body_area, max_scroll);
        self.ring_moving = ring_moving;
        if ring_moving {
            self.mark_dirty();
        }
        let ms = |a: std::time::Instant, b: std::time::Instant| (b - a).as_secs_f32() * 1000.0;
        self.phase_ms = [
            ms(prof0, prof1),
            ms(prof1, prof2),
            ms(prof2, prof3),
            ms(prof3, std::time::Instant::now()),
        ];
    }

    fn render_body(&mut self, p: &mut Painter, active: Screen, body: Rect, scroll: f32) -> f32 {
        match active {
            Screen::Chat => {
                let pills = self.data.quick_replies();
                let quick: Vec<&str> = pills.iter().map(String::as_str).collect();
                screens::chat(
                    p,
                    &*self.data,
                    body,
                    scroll,
                    &self.composer,
                    self.keyboard_open,
                    &quick,
                    self.chat_top_inset,
                    match self.overlay {
                        Overlay::Voice {
                            level, start_ms, ..
                        } => Some((self.now_ms.saturating_sub(start_ms) / 1000, level)),
                        _ => None,
                    },
                    self.appear.map(|(start, from)| {
                        let t = self.now_ms.saturating_sub(start) as f32 / APPEAR_MS as f32;
                        (from, goblin_ui_kit::anim::ease_out_cubic(t))
                    }),
                    &mut self.pics,
                )
            }
            Screen::Feed => screens::feed(p, &*self.data, body, scroll),
            Screen::Ideas => screens::ideas(p, &*self.data, body, scroll),
            Screen::Goals => screens::goals(p, &*self.data, body, scroll),
            Screen::Library => screens::library(p, &*self.data, body, scroll),
            Screen::SideChats => screens::side_chats(p, &*self.data, body, scroll),
            Screen::Notifications => screens::notifications(p, &*self.data, body, scroll),
            Screen::Status => screens::status(p, &*self.data, body, scroll),
            Screen::Settings => {
                let snap = screens::SettingsSnapshot {
                    size: self.theme.ui_size.label(),
                    mode: if self.theme.mode == Mode::Dark {
                        "Dark"
                    } else {
                        "Light"
                    },
                    chat_theme: self.theme.chat_theme.label,
                    avatar: self.theme.avatar.label(),
                    call_greeting: self.setting_bool("voice.call_greeting", false),
                    lights: self.lights_on(),
                    vibrate: self.setting_bool("haptics.enabled", true),
                    keep_connected: self.setting_bool("background.keep_connected", true),
                    autostart: self.data.autostart(),
                    profiles: self.data.profiles(),
                    consent: self.data.consent(),
                    voice_to_voice: self.data.voice_to_voice(),
                    reduce_motion: self.reduce_motion,
                };
                screens::settings(p, &snap, body, scroll)
            }
            Screen::Appearance => screens::appearance(p, &self.theme, body, scroll),
            Screen::InputTest => 0.0,
        }
    }

    fn render_overlay(&mut self, p: &mut Painter, bounds: Rect) {
        match &self.overlay {
            Overlay::None => {
                if self.keyboard_open && self.active() == Screen::Chat {
                    let kbh = p.keyboard_height();
                    let kb_rect = Rect::new(
                        bounds.x,
                        bounds.bottom() - self.theme.metrics.hintbar - kbh,
                        bounds.w,
                        kbh,
                    );
                    p.keyboard(kb_rect, &self.kb, &self.composer);
                }
            }
            Overlay::Voice {
                level, start_ms, ..
            } => {
                // On Chat the composer itself is the recorder. Elsewhere, the same bar floats
                // above the hint bar.
                if self.active() != Screen::Chat {
                    let m = self.theme.metrics;
                    let bar = Rect::new(
                        bounds.x + m.pad,
                        bounds.bottom() - m.hintbar - m.row - m.pad,
                        bounds.w - m.pad * 2.0,
                        m.row,
                    );
                    p.canvas.fill_rrect(bar, bar.h * 0.5, p.theme.pal.surface);
                    p.recorder_bar(bar, self.now_ms.saturating_sub(*start_ms) / 1000, *level);
                }
            }
            Overlay::Confirm {
                title,
                message,
                confirm,
                danger,
                focus,
                ..
            } => {
                let dlg = ConfirmDialog {
                    title,
                    message,
                    confirm,
                    cancel: "Cancel",
                    danger: *danger,
                    focus: *focus,
                };
                p.confirm_dialog(bounds, &dlg);
            }
            Overlay::Profile { focus, armed } => {
                let entry = self
                    .data
                    .profiles()
                    .into_iter()
                    .find(|e| e.name == self.sheet_profile);
                if let Some(entry) = entry {
                    draw_profile_sheet(p, bounds, &entry, *focus, *armed);
                }
            }
            Overlay::Pairing => {
                if let Some(view) = self.data.pairing() {
                    draw_pairing(p, bounds, &view);
                }
            }
            Overlay::Viewer { index } => {
                let m = self.theme.metrics;
                let area = Rect::new(bounds.x, bounds.y, bounds.w, bounds.h - m.hintbar);
                p.canvas.fill_rect(area, Color::BLACK);
                let item = self.viewer_items.get(*index).cloned().unwrap_or_default();
                // The playing clip's frame, else the picture (or the clip's poster).
                let fit = |w: u32, h: u32| {
                    let s = (area.w / w.max(1) as f32).min(area.h / h.max(1) as f32);
                    let (dw, dh) = (w as f32 * s, h as f32 * s);
                    Rect::new(area.cx() - dw * 0.5, area.cy() - dh * 0.5, dw, dh)
                };
                let mut shown = false;
                if let Some(frame) = &self.player_frame {
                    p.canvas.draw_image(frame, fit(frame.w, frame.h), 0.0, 1.0);
                    shown = true;
                } else if let Some(img) = item.picture.as_deref().and_then(|x| self.pics.get(x)) {
                    let dst = fit(img.w, img.h);
                    p.canvas.draw_image(img, dst, 0.0, 1.0);
                    shown = true;
                }
                if !shown {
                    p.line_centered(
                        if item.is_video {
                            "Clip"
                        } else {
                            "Loading the picture…"
                        },
                        TextRole::Body,
                        area,
                        Color::WHITE.fade(0.7),
                    );
                }
                if item.is_video && self.player.is_none() {
                    let d = m.row * 1.6;
                    p.canvas
                        .fill_circle(area.cx(), area.cy(), d * 0.5, Color::BLACK.fade(0.55));
                    let disc = Rect::new(area.cx() - d * 0.5, area.cy() - d * 0.5, d, d);
                    p.icon_centered(Icon::ChevronRight, d * 0.7, disc, Color::WHITE);
                }
                // How far the clip has got, as a line along the bottom, once its length is known.
                if let (true, Some(len)) = (self.player_frame.is_some(), self.clip_secs) {
                    let t = (self.clip_pos / len.max(0.1)).clamp(0.0, 1.0);
                    let h = (m.hairline * 3.0).max(3.0);
                    let track = Rect::new(area.x, area.bottom() - h, area.w, h);
                    p.canvas.fill_rect(track, Color::WHITE.fade(0.25));
                    p.canvas
                        .fill_rect(Rect::new(track.x, track.y, track.w * t, h), Color::WHITE);
                }
                // Caption: what it is, and where in the set.
                let n = self.viewer_items.len();
                let caption = if n > 1 {
                    format!("{}  ·  {} of {}", item.label, index + 1, n)
                } else {
                    item.label.clone()
                };
                if !caption.trim().is_empty() {
                    let cw =
                        (p.measure(&caption, TextRole::Caption) + m.pad * 2.0).min(area.w * 0.9);
                    let pill = Rect::new(
                        area.cx() - cw * 0.5,
                        area.bottom() - m.row * 0.9,
                        cw,
                        m.caption * 1.8,
                    );
                    p.canvas
                        .fill_rrect(pill, pill.h * 0.5, Color::BLACK.fade(0.6));
                    p.line_centered(&caption, TextRole::Caption, pill, Color::WHITE);
                }
            }
            Overlay::Panel { focus } => {
                self.render_panel(p, bounds, *focus);
            }
        }
    }

    /// The navigation panel: the agent, the tabs, the side chats with unread dots, then the
    /// device entries. It slides in from the left over a dimmed screen.
    fn render_panel(&self, p: &mut Painter, bounds: Rect, focus: usize) {
        let m = self.theme.metrics;
        let v = self.panel_anim.value().clamp(0.0, 1.0);
        let dim = if self.theme.mode == Mode::Dark {
            0.6
        } else {
            0.4
        };
        // The hint bar stays clear: it names the panel's buttons.
        p.canvas.fill_rect(
            Rect::new(bounds.x, bounds.y, bounds.w, bounds.h - m.hintbar),
            Color::BLACK.fade(dim * v),
        );
        let w = (bounds.w * 0.62).min(m.grid * 48.0).round();
        let area = Rect::new(
            (bounds.x - w * (1.0 - v)).round(),
            bounds.y,
            w,
            bounds.h - m.hintbar,
        );
        p.canvas.fill_rect(area, p.theme.pal.surface);
        p.canvas.fill_rect(
            Rect::new(area.right(), area.y, m.hairline, area.h),
            p.theme.pal.divider,
        );
        let pad = m.pad;
        // Header: the agent.
        let hh = m.row * 1.15;
        let av = hh * 0.72;
        let av_rect = Rect::new(area.x + pad, area.y + (hh - av) * 0.5, av, av);
        match self.avatar.as_ref().and_then(|a| a.still.as_ref()) {
            // The agent's own picture, as in the chat header.
            Some(img) => p.canvas.draw_image(img, av_rect, av * 0.5, 1.0),
            None => {
                let mascot = p.ui.mascot;
                mascot.draw(p.canvas, p.theme, av_rect, p.now_ms);
            }
        }
        p.line(
            self.data.agent_name(),
            TextRole::Title,
            av_rect.right() + pad * 0.6,
            area.y + hh * 0.5 - m.title * 0.62,
            area.w - av - pad * 3.0,
            p.theme.pal.text,
        );

        // Rows, with a small label above each section. Laid out first, so the list can be
        // scrolled to keep the focused row in view.
        let items = self.panel_items();
        let rowh = m.row;
        let labelh = m.caption * 2.0;
        let mut ys = Vec::with_capacity(items.len());
        let mut labels: Vec<(f32, &str)> = Vec::new();
        let mut y = 0.0;
        for (i, item) in items.iter().enumerate() {
            let section = match item {
                PanelItem::Tab(0) => Some(if self.tabs().len() > 1 {
                    "TABS"
                } else {
                    "CHAT"
                }),
                PanelItem::NewChat => Some("SIDE CHATS"),
                PanelItem::Inbox => Some("THIS DEVICE"),
                _ => None,
            };
            if let Some(s) = section {
                if i > 0 {
                    y += pad * 0.4;
                }
                labels.push((y, s));
                y += labelh;
            }
            ys.push(y);
            y += rowh;
        }
        let list = Rect::new(area.x, area.y + hh, area.w, area.h - hh);
        let fy = ys.get(focus).copied().unwrap_or(0.0);
        // Keep the focused row inside the list, with a row of context where there is one.
        let scroll = (fy + rowh * 2.0 - list.h)
            .max(0.0)
            .min((y - list.h).max(0.0));
        let clip = p.canvas.push_clip(list);
        for (ly, text) in &labels {
            p.line(
                text,
                TextRole::Small,
                list.x + pad,
                list.y + ly - scroll + labelh * 0.3,
                list.w - pad * 2.0,
                p.theme.pal.text_muted,
            );
        }
        let tabs = self.tabs();
        let sides = self.data.side_chats();
        let active_chat = self.data.active_chat();
        let unseen = self.data.notifications().iter().filter(|n| !n.seen).count();
        for (i, item) in items.iter().enumerate() {
            let r = Rect::new(
                list.x + pad * 0.5,
                list.y + ys[i] - scroll,
                list.w - pad,
                rowh,
            );
            if r.bottom() < list.y || r.y > list.bottom() {
                continue;
            }
            let count;
            let (icon, title, here, dot, value): (Icon, &str, bool, bool, &str) = match item {
                PanelItem::Tab(t) => {
                    let (screen, tab) = &tabs[*t];
                    let main = *screen == Screen::Chat;
                    let here = self.stack.is_empty()
                        && self.tab == *t
                        && (!main || active_chat == MAIN_CHAT);
                    (
                        tab.icon,
                        if main { "Main chat" } else { tab.label },
                        here,
                        false,
                        "",
                    )
                }
                PanelItem::NewChat => (Icon::Plus, "New chat", false, false, ""),
                PanelItem::Side(s) => {
                    let c = &sides[*s];
                    let here = self.active() == Screen::Chat && c.id == active_chat;
                    (Icon::Chat, c.title.as_str(), here, c.unread, "")
                }
                PanelItem::Inbox => {
                    count = if unseen > 0 {
                        unseen.to_string()
                    } else {
                        String::new()
                    };
                    (Icon::Bell, "Inbox", false, false, count.as_str())
                }
                PanelItem::Status => (Icon::Activity, "Status", false, false, ""),
                PanelItem::Settings => (Icon::Settings, "Settings", false, false, ""),
                PanelItem::InputTest => (Icon::Keyboard, "Input tester", false, false, ""),
                PanelItem::Quit => (Icon::LogOut, "Quit to launcher", false, false, ""),
            };
            if here {
                // Where the owner is now.
                p.canvas.fill_rrect(r, rowh * 0.3, p.theme.pal.subtle);
            }
            let row = ListRow {
                icon: Some(icon),
                icon_color: (*item == PanelItem::NewChat).then_some(p.theme.pal.accent),
                title,
                subtitle: "",
                chevron: false,
            };
            p.list_row(r, &row, i == focus);
            if dot {
                let d = m.caption * 0.5;
                p.canvas.fill_circle(
                    r.right() - pad - d * 0.5,
                    r.cy(),
                    d * 0.5,
                    p.theme.pal.accent,
                );
            }
            if !value.is_empty() {
                let vw = p.measure(value, TextRole::Caption) + pad;
                let pill = Rect::new(
                    r.right() - pad * 0.6 - vw,
                    r.cy() - m.caption * 0.85,
                    vw,
                    m.caption * 1.7,
                );
                p.canvas.fill_rrect(pill, pill.h * 0.5, p.theme.pal.accent);
                p.line_centered(value, TextRole::Caption, pill, p.theme.pal.on_accent);
            }
        }
        p.canvas.set_clip(clip);
    }

    fn hints_for(&self, active: Screen) -> Hints {
        if self.keyboard_open {
            return Hints::new()
                .add(Button::A, "Type")
                .add(Button::X, "Delete")
                .add(Button::Y, "Space")
                .add(Button::L1, "Shift")
                .add(Button::R1, "Symbols")
                .add(
                    Button::Start,
                    if self.renaming.is_some() {
                        "Save name"
                    } else {
                        "Send"
                    },
                )
                .add(Button::B, "Close");
        }
        if !matches!(self.overlay, Overlay::None) {
            return match self.overlay {
                Overlay::Confirm { .. } => Hints::new()
                    .add(Button::A, "Choose")
                    .add(Button::B, "Cancel"),
                Overlay::Profile { .. } => Hints::new()
                    .add(Button::A, "Choose")
                    .add(Button::B, "Close"),
                Overlay::Pairing => {
                    if self.data.pairing().map_or(true, |p| p.step.ended()) {
                        Hints::new().add(Button::A, "Done")
                    } else {
                        Hints::new().add(Button::B, "Stop pairing")
                    }
                }
                Overlay::Panel { focus } => {
                    let h = Hints::new()
                        .add(Button::A, "Open")
                        .add(Button::X, "New chat");
                    let on_side = matches!(self.panel_items().get(focus), Some(PanelItem::Side(_)));
                    if on_side {
                        h.add(Button::Select, "Rename")
                            .add(Button::Y, "Forget")
                            .add(Button::B, "Close")
                    } else {
                        h.add(Button::B, "Close")
                    }
                }
                Overlay::Viewer { index } => {
                    let mut h = Hints::new();
                    if self.viewer_items.len() > 1 {
                        h = h.add(Button::Left, "Next");
                    }
                    if self.viewer_items.get(index).is_some_and(|m| m.is_video) {
                        h = h.add(
                            Button::A,
                            if self.player.is_some() {
                                "Pause"
                            } else if self.clip_paused.is_some() {
                                "Resume"
                            } else {
                                "Play"
                            },
                        );
                    }
                    h.add(
                        Button::B,
                        if self.player.is_some() || self.clip_paused.is_some() {
                            "Stop"
                        } else {
                            "Close"
                        },
                    )
                }
                Overlay::Voice { .. } => Hints::new()
                    .add(Button::F1, "Release to send")
                    .add(Button::B, "Discard"),
                _ => Hints::new().add(Button::B, "Cancel"),
            };
        }
        // One vocabulary everywhere: A = Select, B = Back, X = the screen's main extra action,
        // Y = its second, L2/R2 = page, Select = inbox, Menu = menu. Only what works is shown;
        // the bar drops what does not fit, so the most useful come first.
        match active {
            Screen::Chat => {
                let mut h = Hints::new();
                if self.chat_scrolled_back() {
                    h = h.add(Button::B, "Newest");
                } else if self.data.active_chat() != MAIN_CHAT {
                    h = h.add(Button::B, "Main chat");
                }
                let h = h;
                // Type (X), Talk (F1) and Send (Start) are labelled on the composer itself.
                let on_call = self.data.call().is_some();
                h.add(Button::A, "Select")
                    .add(Button::Y, "Chats")
                    .add(Button::F2, if on_call { "Hang up" } else { "Call" })
                    .add(Button::L2, "")
                    .add(Button::R2, "Scroll")
                    .add(Button::Select, "Inbox")
                    .add(Button::Menu, "More")
            }
            Screen::Settings if self.settings_focus_opens => Hints::new()
                .add(Button::A, "Open")
                .add(Button::B, "Back")
                .add(Button::L2, "")
                .add(Button::R2, "Page"),
            Screen::Settings | Screen::Appearance => Hints::new()
                .add(Button::Left, "Change")
                .add(Button::B, "Back")
                .add(Button::L2, "")
                .add(Button::R2, "Page"),
            Screen::SideChats => Hints::new()
                .add(Button::A, "Open")
                .add(Button::B, "Back")
                .add(Button::X, "New chat")
                .add(Button::L2, "")
                .add(Button::R2, "Page"),
            Screen::Notifications => Hints::new()
                .add(Button::A, "Open")
                .add(Button::B, "Back")
                .add(Button::Y, "Mark all read")
                .add(Button::Select, "Status")
                .add(Button::L2, "")
                .add(Button::R2, "Page"),
            Screen::Status => Hints::new()
                .add(Button::B, "Back")
                .add(Button::L2, "")
                .add(Button::R2, "Page"),
            _ => Hints::new()
                .add(Button::A, "Open")
                .add(Button::F1, "Talk")
                .add(Button::F2, "Call")
                .add(Button::Select, "Inbox")
                .add(Button::Menu, "More"),
        }
    }

    fn finish_focus(&mut self, ui: &mut Ui, key: u64) {
        self.ensure_focus(ui);
        if let Some(id) = ui.focus.current() {
            ui.memory.set(key, id);
        }
    }

    /// Focuses the first item when nothing valid is focused. The ring is drawn during the frame,
    /// so a focus picked here needs one more frame to show.
    fn ensure_focus(&mut self, ui: &mut Ui) {
        let before = ui.focus.current();
        if ui.focus.ensure() != before {
            self.mark_dirty();
        }
    }

    fn finish_focus_scrolled(&mut self, ui: &mut Ui, key: u64, body: Rect, max_scroll: f32) {
        self.ensure_focus(ui);
        if let Some(id) = ui.focus.current() {
            ui.memory.set(key, id);
        }
        // Bring the focused item into view when focus moves, not on every frame: otherwise an
        // item that stays focused holds the view and the screen cannot be scrolled past it
        // (reading back through a chat with a picture focused).
        let now = ui.focus.current().map(|id| (key, id));
        let moved = now != self.focus_scrolled;
        // While the scroll is still easing to where the last move aimed, keep aiming.
        let settling = self.focus_settling && self.scroll_easing();
        self.focus_scrolled = now;
        self.focus_settling = moved || settling;
        if !self.focus_settling {
            // Scrolled until the focused item left the screen (reading back through a chat):
            // focus goes to the nearest item still on it, so A never acts on something unseen.
            if key == Screen::Chat.key() {
                if let Some(r) = ui.focus.focused_rect() {
                    if r.bottom() < body.y || r.y > body.bottom() {
                        let nearest = ui
                            .focus
                            .items()
                            .iter()
                            .filter(|f| f.rect.bottom() > body.y && f.rect.y < body.bottom())
                            .min_by(|a, b| {
                                let d = |f: &goblin_ui_kit::focus::Focusable| {
                                    (f.rect.cy() - r.cy()).abs()
                                };
                                d(a).total_cmp(&d(b))
                            })
                            .map(|f| f.id);
                        if let Some(id) = nearest {
                            ui.focus.set(id);
                            ui.memory.set(key, id);
                            self.focus_scrolled = Some((key, id));
                            self.mark_dirty();
                        }
                    }
                }
            }
            return;
        }
        let delta = ui.focus.scroll_into_view(body, self.theme.metrics.pad);
        if delta.abs() > 0.5 {
            // The focused item's position was drawn with the eased scroll, so aim from there.
            let old = self.scroll_shown_of(key);
            let s = (old + delta).clamp(0.0, max_scroll);
            // Redraw only if the scroll really moved. A focused item outside the scroll area
            // (e.g. the tab rail) asks for a delta that clamping cancels; marking dirty anyway
            // redrew every idle tick (~18% CPU on the Brick).
            if (s - self.scroll_of(key)).abs() > 0.5 {
                self.set_scroll(key, s);
                if s < max_scroll {
                    self.pin_bottom = None; // focus moved up: the user is reading back
                }
                self.mark_dirty();
            }
        }
    }

    /// Draws the FPS overlay (called by the shell after measuring the frame).
    pub fn draw_fps(&self, canvas: &mut Canvas, ui: &mut Ui, now_ms: u64) {
        if !self.show_fps {
            return;
        }
        let mut p = Painter::new(canvas, ui, &self.theme, now_ms);
        let m = self.theme.metrics;
        let txt = format!(
            "{:.1} ms  {:.0} fps",
            self.frame_ms,
            1000.0 / self.frame_ms.max(0.1)
        );
        let w = p.measure(&txt, TextRole::Caption) + m.pad;
        let r = Rect::new(2.0, 2.0, w, m.caption * 2.0);
        p.canvas
            .fill_rrect(r, m.radius_card * 0.4, Color::BLACK.fade(0.6));
        p.line(
            &txt,
            TextRole::Caption,
            r.x + m.pad * 0.5,
            r.cy() - m.caption * 0.6,
            w,
            Color::WHITE,
        );
    }
}

/// The local time as `h:mm` (12-hour, as the app shows it). Uses the C library's local time so
/// the firmware's time zone applies.
fn local_clock() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or(0);
    // SAFETY: localtime_r writes only into `tm`, which we own.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&now, &mut tm) }.is_null() {
        return String::new();
    }
    let hour = match tm.tm_hour % 12 {
        0 => 12,
        h => h,
    };
    format!("{hour}:{:02}", tm.tm_min)
}

/// The system volume bar: a pill under the top bar with the output icon, a level bar and the
/// percentage ("Muted" when the mute switch silences it).
fn draw_volume_hud(p: &mut Painter, bounds: Rect, v: goblin_platform::sysvol::SystemVolume) {
    let m = p.m();
    let w = (bounds.w * 0.6).min(m.grid * 44.0);
    let h = m.grid * 5.5;
    let rect = Rect::new(
        bounds.cx() - w * 0.5,
        bounds.y + m.topbar + m.pad * 0.4,
        w,
        h,
    );
    p.canvas.fill_rrect(rect, h * 0.5, p.theme.pal.surface);
    p.canvas
        .stroke_rrect(rect, h * 0.5, m.hairline, p.theme.pal.divider);
    let ic = m.body * 1.3;
    let icon = if v.headphones {
        Icon::Headphones
    } else {
        Icon::Volume
    };
    let colour = if v.audible() {
        p.theme.pal.text
    } else {
        p.theme.pal.text_muted
    };
    p.icon(icon, ic, rect.x + m.pad, rect.cy() - ic * 0.5, colour);
    let label = if !v.audible() {
        "Muted".to_owned()
    } else {
        format!("{}%", v.percent)
    };
    let lw = p.measure("100%", TextRole::Caption);
    p.line(
        &label,
        TextRole::Caption,
        rect.right() - m.pad - lw,
        rect.cy() - m.caption * 0.6,
        lw + 4.0,
        colour,
    );
    let bx = rect.x + m.pad * 1.6 + ic;
    let bw = rect.right() - m.pad * 1.6 - lw - bx;
    let bh = m.grid * 0.9;
    let track = Rect::new(bx, rect.cy() - bh * 0.5, bw, bh);
    p.canvas.fill_rrect(track, bh * 0.5, p.theme.pal.subtle);
    let fill = bw * (v.percent as f32 / 100.0);
    if fill > 0.0 {
        p.canvas.fill_rrect(
            Rect::new(bx, track.y, fill.max(bh), bh),
            bh * 0.5,
            p.theme.pal.accent,
        );
    }
}

/// A small status pill under the top bar.
fn draw_pill(p: &mut Painter, bounds: Rect, icon: Icon, text: &str) {
    let m = p.m();
    let ic = m.body * 1.2;
    let tw = p.measure(text, TextRole::Caption);
    let w = tw + ic + m.pad * 3.0;
    let h = m.grid * 5.0;
    let rect = Rect::new(
        bounds.cx() - w * 0.5,
        bounds.y + m.topbar + m.pad * 0.4,
        w,
        h,
    );
    p.canvas.fill_rrect(rect, h * 0.5, p.theme.pal.surface);
    p.canvas
        .stroke_rrect(rect, h * 0.5, m.hairline, p.theme.pal.divider);
    p.icon(
        icon,
        ic,
        rect.x + m.pad,
        rect.cy() - ic * 0.5,
        p.theme.pal.accent,
    );
    p.line(
        text,
        TextRole::Caption,
        rect.x + m.pad * 2.0 + ic,
        rect.cy() - m.caption * 0.6,
        tw + 4.0,
        p.theme.pal.text,
    );
}

/// The settings value (`appearance.avatar_size`) for an avatar size.
fn avatar_id(a: AvatarSize) -> &'static str {
    match a {
        AvatarSize::Hidden => "hidden",
        AvatarSize::Small => "small",
        AvatarSize::Medium => "medium",
        AvatarSize::Large => "large",
        AvatarSize::ExtraLarge => "xl",
    }
}

/// The profile sheet: the profile, what it is, and what can be done with it.
fn draw_profile_sheet(
    p: &mut Painter,
    bounds: Rect,
    entry: &crate::data::ProfileEntry,
    focus: usize,
    armed: Option<usize>,
) {
    let m = p.m();
    let actions = profile_actions(entry);
    p.scrim(bounds);
    let pad = m.pad * 1.5;
    let w = (bounds.w * 0.64).min(m.grid * 46.0);
    let rowh = m.row;
    let head = m.title * 1.6 + m.body * 1.8;
    let note_h = m.caption * 3.2;
    let h = pad * 2.0 + head + rowh * actions.len() as f32 + note_h;
    let card = bounds.centered(w, h);
    p.canvas
        .fill_rrect(card, m.radius_card, p.theme.pal.surface);
    let inner = w - pad * 2.0;
    p.line(
        &entry.name,
        TextRole::Title,
        card.x + pad,
        card.y + pad,
        inner,
        p.theme.pal.text,
    );
    p.line(
        screens::profile_state(entry),
        TextRole::Body,
        card.x + pad,
        card.y + pad + m.title * 1.6,
        inner,
        p.theme.pal.text_muted,
    );
    let mut y = card.y + pad + head;
    for (i, (action, label)) in actions.iter().enumerate() {
        let r = Rect::new(card.x + pad * 0.5, y, w - pad, rowh);
        let danger = matches!(action, ProfileAction::Unpair | ProfileAction::Delete);
        if i == focus {
            p.canvas
                .fill_rrect(r, m.radius_card * 0.6, p.theme.pal.subtle_strong);
        }
        let text = if armed == Some(i) {
            "Press A again to confirm"
        } else {
            label
        };
        let color = if danger {
            p.theme.pal.danger
        } else {
            p.theme.pal.text
        };
        p.line(
            text,
            TextRole::Body,
            r.x + pad * 0.5,
            r.cy() - m.body * 0.65,
            r.w - pad,
            color,
        );
        y += rowh;
    }
    let note = if entry.active {
        "Each profile is one Muse account, with its own chats, files and permissions."
    } else {
        "Using it closes this account's chats here and opens that one's."
    };
    p.paragraph(
        note,
        TextRole::Caption,
        card.x + pad,
        y + m.caption * 0.6,
        inner,
        p.theme.pal.text_muted,
    );
}

/// Pairing: what to do in the Muse app, the name to choose there, and how far it has got.
fn draw_pairing(p: &mut Painter, bounds: Rect, view: &crate::data::PairingView) {
    use crate::data::PairStep;
    let m = p.m();
    p.scrim(bounds);
    let pad = m.pad * 1.5;
    let w = (bounds.w * 0.78).min(m.grid * 56.0);
    let needs_token = view.step == PairStep::Failed && view.detail.starts_with("No SDK token");
    let h = if needs_token {
        m.title * 2.0 + m.body * 9.5 + pad * 2.0
    } else if view.step.ended() {
        m.title * 2.0 + m.body * 6.0 + pad * 2.0
    } else {
        m.title * 2.0 + m.body * 9.5 + m.row * 1.15 + pad * 2.0
    };
    let card = bounds.centered(w, h);
    p.canvas
        .fill_rrect(card, m.radius_card, p.theme.pal.surface);
    let inner = w - pad * 2.0;
    let mut y = card.y + pad;
    p.line(
        &format!("Pair \u{201c}{}\u{201d} with the Muse app", view.profile),
        TextRole::Title,
        card.x + pad,
        y,
        inner,
        p.theme.pal.text,
    );
    y += m.title * 2.0;
    let (state, color) = match view.step {
        PairStep::Opening => ("Turning Bluetooth on…", p.theme.pal.text_muted),
        PairStep::Waiting => ("Waiting for the Muse app", p.theme.pal.text_muted),
        PairStep::Connected => ("The app is connected", p.theme.pal.accent),
        PairStep::Confirmed => (
            "Confirmed. In the app, choose \u{201c}Use current connection\u{201d}",
            p.theme.pal.accent,
        ),
        PairStep::Checking => ("Checking with Muse…", p.theme.pal.accent),
        PairStep::Done => ("Paired", p.theme.pal.accent),
        PairStep::Failed if needs_token => ("An SDK token is needed first", p.theme.pal.text),
        PairStep::Failed => ("Pairing did not work", p.theme.pal.danger),
        PairStep::Cancelled => ("Pairing stopped", p.theme.pal.text_muted),
        PairStep::TimedOut => ("Nobody connected in time", p.theme.pal.text_muted),
    };
    if !view.step.ended() {
        let steps = "On your phone, open the Muse app, then Settings, Devices and the + \
                     at the top. Choose this name:";
        y += p.paragraph(
            steps,
            TextRole::Body,
            card.x + pad,
            y,
            inner,
            p.theme.pal.text,
        );
        y += m.body * 0.9;
        let name = if view.ble_name.is_empty() {
            "…"
        } else {
            &view.ble_name
        };
        let pill = Rect::new(card.x + pad, y, inner, m.row * 1.15);
        p.canvas
            .fill_rrect(pill, m.radius_card * 0.6, p.theme.pal.subtle_strong);
        p.line_centered(name, TextRole::Title, pill, p.theme.pal.text);
        y += pill.h + m.body * 1.1;
    }
    p.line(state, TextRole::Body, card.x + pad, y, inner, color);
    y += m.body * 1.9;
    let note = match view.step {
        // The line above already says a token is missing; show what to do about it.
        PairStep::Failed if needs_token => view
            .detail
            .split_once(". ")
            .map_or(view.detail.as_str(), |(_, rest)| rest),
        PairStep::Failed => view.detail.as_str(),
        PairStep::Done => "This profile can now talk to that Muse account.",
        s if s.ended() => "",
        _ => {
            "If the app says it could not connect, tap Connect again; the first try often \
              misses. The app will warn that this is a community device."
        }
    };
    if !note.is_empty() {
        // The way to get a token is the thing to read, not a footnote.
        let (role, color) = if needs_token {
            (TextRole::Body, p.theme.pal.text)
        } else {
            (TextRole::Caption, p.theme.pal.text_muted)
        };
        p.paragraph(note, role, card.x + pad, y, inner, color);
    }
}

/// A cheap identity for a picture: its size and a spread of its pixels.
fn image_key(img: &goblin_ui_kit::Image) -> u64 {
    let px = img.pixels();
    let step = (px.len() / 257).max(1);
    let sample: Vec<u32> = px.iter().step_by(step).copied().collect();
    goblin_ui_kit::widgets::sprite_key(&(img.w, img.h, sample))
}
