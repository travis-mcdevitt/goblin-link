//! Screen draw functions. Each takes a [`Painter`], its data, the body rectangle and a scroll
//! offset, draws the screen (registering focusables at their on-screen positions) and returns the
//! total content height so the router can clamp scrolling.

use goblin_ui_kit::theme::Metrics;
use goblin_ui_kit::widgets::{
    sprite_key, AgentPanel, Bubble, LinkState, Painter, RowKind, SettingRow, TextRole, TurnStatus,
};
use goblin_ui_kit::{Button, Color, Icon, Rect};

use crate::data::{Consent, DataProvider};

/// Owned snapshot of what Settings shows, passed to [`settings`].
pub struct SettingsSnapshot {
    /// Size choice label ("Normal").
    pub size: &'static str,
    /// "Light" or "Dark".
    pub mode: &'static str,
    /// Chat colour label.
    pub chat_theme: &'static str,
    /// Avatar size label.
    pub avatar: &'static str,
    /// Reduce motion.
    pub reduce_motion: bool,
    /// Speak replies to voice notes.
    pub voice_to_voice: bool,
    /// Muse speaks first on a call.
    pub call_greeting: bool,
    /// The profile in use is paired with a Hermes Agent, which has no calls.
    pub hermes: bool,
    /// Blink the lights for replies that arrive in the background.
    pub lights: bool,
    /// Vibrate when a reply arrives.
    pub vibrate: bool,
    /// Connect when the handheld starts (`None`: the device cannot, and the row is left out).
    pub autostart: Option<bool>,
    /// The profiles (accounts) on this handheld.
    pub profiles: Vec<crate::data::ProfileEntry>,
    /// Command consent toggles (enforced by the daemon).
    pub consent: Consent,
    /// What the profile's Muse built and installed, those waiting first.
    pub builds: Vec<goblin_ipc::types::BuildItem>,
    /// The Device page's lines: what it is called and how it stands.
    pub device: Vec<(&'static str, String)>,
}

/// Focus ids of the Settings rows.
pub mod setting_id {
    /// Size.
    pub const SIZE: u64 = 20;
    /// Light / Dark.
    pub const MODE: u64 = 21;
    /// Chat colour.
    pub const CHAT_THEME: u64 = 22;
    /// Avatar size.
    pub const AVATAR: u64 = 23;
    /// Ask Muse to change its avatar (opens the chat with a draft).
    /// Reduce motion.
    pub const REDUCE_MOTION: u64 = 14;
    /// Speak replies to voice notes.
    pub const VOICE_TO_VOICE: u64 = 18;
    /// Muse greets on calls.
    pub const CALL_GREETING: u64 = 24;
    /// Lights for background replies.
    pub const LIGHTS: u64 = 25;
    /// Vibrate on replies.
    pub const VIBRATE: u64 = 26;
    /// Keep connected.
    /// Connect when the handheld starts.
    pub const AUTOSTART: u64 = 28;
    /// How far the file commands reach.
    pub const FILES: u64 = 29;
    /// Add a profile.
    pub const NEW_PROFILE: u64 = 30;
    /// The first profile row; the others follow in order.
    pub const PROFILE_BASE: u64 = 3000;
    /// Whether Muse may install what it builds.
    pub const BUILDS: u64 = 31;
    /// The first of the rows for what Muse built; the rest follow.
    pub const BUILD_BASE: u64 = 4000;
    /// Back to the launcher, the agent still connected.
    pub const LEAVE_KEEP: u64 = 34;
    /// Back to the launcher with nothing left running.
    pub const LEAVE_STOP: u64 = 35;
    /// Consent: shell, file read, file write, drop-ins, writes outside home.
    pub const CONSENT: [u64; 5] = [12, 13, 16, 17, 33];
}

/// Clips to `body` and returns a guard value for restoring the clip.
fn begin(p: &mut Painter, body: Rect) -> goblin_ui_kit::IRect {
    p.canvas.push_clip(body)
}

fn end(p: &mut Painter, clip: goblin_ui_kit::IRect) {
    p.canvas.set_clip(clip);
}

/// The controller button that gives each answer to a request.
pub fn answer_button(answer: &str) -> Option<Button> {
    match answer {
        "no" => Some(Button::B),
        "yes" => Some(Button::A),
        "hour" => Some(Button::Y),
        _ => None,
    }
}

/// What a card says when it cannot show all of a request.
pub const NOT_WHOLE: &str = "Cannot be shown whole";

/// How a request's card lies over the answers: where it is, how much of the request is on
/// it, and so which of its answers may be given from this screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CardLayout {
    /// The card itself.
    pub rect: Rect,
    /// The highest a card may reach: just under the header.
    pub ceiling: f32,
    /// The room it takes over the answers, the gap under it included.
    pub h: f32,
    /// Every word of the request is on the card and each of its answers has a button
    /// under it. The owner can give leave only for what they could read: a card that is
    /// not whole says so, and the app offers it no answer but its "no".
    pub whole: bool,
    /// How many of the answers have a button under the card, from the first.
    pub answers: usize,
    /// Lines of the heading, the body and the facts that are on the card.
    lines: [usize; 3],
    /// The height of a line of the heading, and of the rest.
    per_line: [f32; 2],
}

/// The height of the composer and the row of answers or replies over it.
fn strip_height(m: &Metrics) -> f32 {
    m.row + m.row * 0.9 + m.pad * 2.0
}

/// The width of one answer or reply over the composer, with the button that gives it.
fn chip_width(p: &mut Painter, label: &str, key: Option<Button>) -> f32 {
    let m = p.m();
    let kh = m.row * 0.72 * 0.74;
    let kw = key.map_or(0.0, |b| p.keycap_width(b, kh) + m.pad * 0.4);
    p.measure(label, TextRole::Body) + kw + m.pad * 1.6
}

/// How wide the words of a card may run beside its icon: the heading (and the facts), and
/// the body.
fn card_columns(m: &Metrics, card_w: f32) -> (f32, f32) {
    let isz = m.body * 1.1;
    (card_w - isz - m.pad * 2.2, card_w - m.pad * 2.4 - isz)
}

/// Whether a card shows `text` for what it is: every character in it is one the reader
/// sees. The plain space and the line break are laid out. Any other character must not be
/// one that passes unseen whatever the font (a joiner, a tag, a selector, a space that is
/// not the plain one), and must have a glyph here that puts something on the screen: not
/// a box, and not nothing. A mark that is drawn on the character before it needs one
/// there, on its own line.
fn reads_as_written(fonts: &mut goblin_ui_kit::text::TextSystem, text: &str) -> bool {
    use goblin_ipc::shown::unseen;
    use goblin_ui_kit::text::Drawn;
    let mut before = None;
    text.chars().all(|c| {
        let seen = !unseen(before, c)
            && match c {
                ' ' | '\n' => true,
                _ => match fonts.drawn(c) {
                    Drawn::Glyph => true,
                    Drawn::Mark => before.is_some_and(|b| !matches!(b, ' ' | '\n')),
                    Drawn::Nothing => false,
                },
            };
        before = Some(c);
        seen
    })
}

/// Lays a request's card out in the room there is for it: under the header, which ends
/// `top_inset` below the top of `body`, and over the answers. A card never grows past that
/// room. What does not fit is left off in whole lines, the agent's words before the
/// handheld's own, and the card is then not [`CardLayout::whole`].
pub fn card_layout(
    p: &mut Painter,
    item: &goblin_ipc::types::RequestView,
    body: Rect,
    top_inset: f32,
) -> CardLayout {
    let m = p.m();
    let pad = m.pad;
    let card_w = body.w - pad * 2.0;
    let (head_w, body_w) = card_columns(&m, card_w);
    let (head_n, head_line, head_lost) =
        p.lines_needed(&item.heading, TextRole::BodyStrong, head_w);
    // A card with nothing for a heading still keeps its first line, beside the icon.
    let head_n = head_n.max(1);
    let (body_n, line, body_lost) = p.lines_needed(&item.body, TextRole::Caption, body_w);
    let (facts_n, _, facts_lost) = p.lines_needed(&item.facts, TextRole::Caption, head_w);
    // What even all its lines would not show: spaces at the end of a line, several spaces
    // where a line is carried over, indentation wider than the card.
    let lost = head_lost || body_lost || facts_lost;
    // The answers that have room in their row, as [`chat`] draws them.
    let (mut cx, right) = (body.x + pad, body.right() - pad);
    let mut answers = 0;
    for a in &item.answers {
        let w = chip_width(p, &a.label, answer_button(&a.id));
        if cx + w > right {
            break;
        }
        cx += w + pad * 0.5;
        answers += 1;
    }
    let strip_h = strip_height(&m);
    // Under the header with a little air, and never past the top of the screen.
    let room = (body.h - strip_h - top_inset - pad * 0.5).max(0.0);
    // The card's edges and the space under its heading, then what its last lines (the
    // facts, the note that it is cut) add when there are any.
    let frame = pad * 1.4 + m.body * 1.5 - head_line;
    let tail = |n: usize| {
        if n == 0 {
            m.caption * 0.6
        } else {
            m.caption * 2.0 + (n as f32 - 1.0) * line
        }
    };
    let height = |head: usize, text: usize, last: usize| {
        frame + head as f32 * head_line + text as f32 * line + tail(last)
    };
    // The words on the answers are read too: they say what a press does.
    let readable = [&item.heading, &item.body, &item.facts]
        .into_iter()
        .chain(item.answers.iter().map(|a| &a.label))
        .all(|t| reads_as_written(&mut p.ui.text, t));
    let whole = !item.cut
        && !lost
        && readable
        && answers == item.answers.len()
        && height(head_n, body_n, facts_n) <= room;
    let lines = if whole {
        [head_n, body_n, facts_n]
    } else {
        // The note takes a line. Then the heading, the facts and the body, in that order,
        // take the whole lines there is room for; the heading always has its first.
        let fit = |left: f32, per: f32, most: usize| ((left / per).max(0.0) as usize).min(most);
        let mut left = room - height(0, 0, 1);
        let head = fit(left, head_line, head_n).max(1);
        left -= head as f32 * head_line;
        let facts = fit(left, line, facts_n);
        left -= facts as f32 * line;
        [head, fit(left, line, body_n), facts]
    };
    let h = height(lines[0], lines[1], lines[2] + usize::from(!whole));
    CardLayout {
        rect: Rect::new(
            body.x + pad,
            body.bottom() - strip_h - h,
            card_w,
            h - pad * 0.3,
        ),
        ceiling: body.bottom() - strip_h - room,
        h,
        whole,
        answers,
        lines,
        per_line: [head_line, line],
    }
}

/// The answers a request's card offers on this screen. Nothing is offered that the screen
/// did not show: an approval whose card is not whole (or is not drawn at all) keeps only
/// its "no", and no answer is offered without its button under the card.
pub fn answers_offered(
    item: &goblin_ipc::types::RequestView,
    card: Option<&CardLayout>,
) -> Vec<goblin_ipc::types::RequestAnswer> {
    use goblin_ipc::types::RequestKind;
    let whole = card.is_some_and(|c| c.whole);
    let drawn = card.map_or(0, |c| c.answers);
    item.answers
        .iter()
        .enumerate()
        .filter(|(i, a)| {
            if item.kind == RequestKind::Approval && !whole {
                a.id == "no"
            } else {
                *i < drawn
            }
        })
        .map(|(_, a)| a.clone())
        .collect()
}

/// What the strip of answers (or replies) and the composer looks like, for drawing it once
/// and placing it after. An answer's button is drawn on it, so the answers' own ids are
/// part of it and not only their words: two requests with the same words on their answers
/// can put them on different buttons.
fn strip_key(
    composer: &str,
    quick: &[&str],
    request: Option<&goblin_ipc::types::RequestView>,
    focus: Option<u64>,
    on_call: bool,
    layout_key: u64,
) -> u64 {
    let buttons: Option<Vec<&str>> =
        request.map(|item| item.answers.iter().map(|a| a.id.as_str()).collect());
    sprite_key(&(0xC0u8, composer, quick, buttons, focus, on_call, layout_key))
}

/// The main chat screen with a transcript, a composer and quick-reply chips.
#[allow(clippy::too_many_arguments)]
pub fn chat(
    p: &mut Painter,
    data: &dyn DataProvider,
    body: Rect,
    scroll: f32,
    composer: &str,
    keyboard_open: bool,
    quick: &[&str],
    // The request waiting in this chat, with the answers it offers here, and its card.
    request: Option<(&goblin_ipc::types::RequestView, &CardLayout)>,
    top_inset: f32,
    // The voice note being recorded: how long it has run and may run (ms), and its level.
    recorder: Option<(u64, u64, f32)>,
    appear: Option<(usize, f32)>,
    pics: &mut crate::media::PictureCache,
) -> f32 {
    let m = p.m();
    let pad = m.pad;
    // Reserve the composer + chips strip at the bottom (hidden while the keyboard is open, which
    // draws its own area).
    let strip_h = if keyboard_open { 0.0 } else { strip_height(&m) };
    // A request from Muse sits above the answer buttons until it is answered. It is drawn
    // by the handheld from what is to be installed: nothing Muse writes can look like it.
    let request = request.filter(|_| !keyboard_open);
    let card_h = request.map_or(0.0, |(_, card)| card.h);
    let composer_h = strip_h + card_h;
    // The transcript runs to the top of the screen; the floating header (tabs, avatar, status)
    // is drawn over it, so content starts below `top_inset`.
    let header_h = 0.0;
    let transcript = Rect::new(body.x, body.y, body.w, (body.h - composer_h).max(0.0));

    let clip = begin(p, transcript);
    let content_w = transcript.w - pad * 2.0;
    let first_y = transcript.y + top_inset + pad * 0.5 - scroll;
    let mut next_y = first_y;
    let mut media_index: u64 = 0;
    let turns = data.main_chat();
    // A long chat is mostly off screen. Each message's height is remembered from when it
    // was last drawn, so one that lies wholly above or below the screen is stepped over:
    // no text is laid out and no picture decoded for it. What changes a height changes its
    // key, and then it is drawn (clipped) once to measure it again.
    if pics.heights.len() > 4000 {
        pics.heights.clear();
    }
    // A tuple hashes its fields in order, so this matches hashing them one by one.
    let layout_key = sprite_key(&(content_w.to_bits(), m.body.to_bits(), m.row.to_bits()));
    let focused_now = p.ui.focus.current();
    let (screen_top, screen_bottom) = (transcript.y - pad, transcript.bottom() + pad);
    for (idx, turn) in turns.iter().enumerate() {
        // A message that has just arrived rises a little and fades in.
        let rising = appear.filter(|(from, _)| idx >= *from).map(|(_, t)| t);
        let lift = rising.map_or(0.0, |t| (1.0 - t) * pad * 1.2);
        // A centred time label opens each group of messages (a gap of 15 minutes or more).
        let prev_at = if idx == 0 { 0 } else { turns[idx - 1].at };
        let labelled = turn.at > 0 && (idx == 0 || turn.at.saturating_sub(prev_at) >= GROUP_GAP_MS);
        let key = turn_key(turn, layout_key);
        let media_ids = MEDIA_FOCUS_BASE + media_index
            ..MEDIA_FOCUS_BASE + media_index + turn.media.len() as u64;
        let holds_focus = focused_now.is_some_and(|id| media_ids.contains(&id));
        // The label's height is the same for every label at this text size.
        let label_h = pics.heights.get(&layout_key).copied();
        if let (Some(&h), None, false) = (pics.heights.get(&key), rising, holds_focus) {
            let lead = if labelled { label_h } else { Some(0.0) };
            if let Some(lead) = lead {
                let (top, bottom) = (next_y, next_y + lead + h);
                if bottom < screen_top || top > screen_bottom {
                    media_index += turn.media.len() as u64;
                    next_y = bottom + pad * 0.9;
                    continue;
                }
            }
        }
        if labelled {
            let label = time_label(turn.at, wall_now_ms());
            let divider = |p: &mut Painter, r: Rect| {
                let area = Rect::new(r.x + pad, r.y, content_w, 0.0);
                p.time_divider(area, &label) + pad * 0.3
            };
            // Its height is wanted before it is drawn (to say how large its sprite is).
            let known = match label_h {
                Some(h) => h,
                None => p.muted(|p| divider(p, Rect::new(transcript.x, next_y, 0.0, 0.0))),
            };
            let lead = p.sprite(
                sprite_key(&(0xD1u8, &label, layout_key)),
                Rect::new(transcript.x, next_y, transcript.w, known),
                false,
                divider,
            );
            pics.heights.insert(layout_key, lead);
            next_y += lead;
        }
        let top = next_y;
        let y = top + lift;
        let area = Rect::new(transcript.x + pad, y, content_w, 0.0);
        // The bubble (or Muse's panel) and the reaction hanging off it.
        let bubble = |p: &mut Painter, area: Rect| {
            let h = if !turn.media.is_empty() && turn.body.trim().is_empty() {
                0.0 // media only: no empty bubble
            } else if turn.from_user {
                let b = Bubble {
                    text: &turn.body,
                    status: turn.status,
                };
                p.user_bubble(area, &b)
            } else {
                let panel = AgentPanel {
                    markdown: &turn.body,
                    streaming: turn.status == TurnStatus::Replying,
                };
                if turn.status == TurnStatus::Replying && turn.body.is_empty() {
                    p.typing_bubble(area)
                } else {
                    p.agent_panel(area, &panel)
                }
            };
            match turn.reaction.as_deref() {
                Some(emoji) if h > 0.0 => h + p.reaction_pill(emoji),
                _ => h,
            }
        };
        // A message that is settled looks the same wherever it is: it is drawn once and
        // then only moved. One still arriving (rising, or Muse still writing it) changes
        // every frame and is drawn in place.
        let settled = rising.is_none() && turn.status != TurnStatus::Replying;
        let bubble_key = key ^ 0xB0BB_1E00_0000_0001;
        let mut h = if settled {
            let known = match pics.heights.get(&bubble_key) {
                Some(&h) => h,
                None => p.muted(|p| bubble(p, area)),
            };
            pics.heights.insert(bubble_key, known);
            if known > 0.0 {
                // Room around it for what a bubble draws past its own edge.
                let room = (pad * 0.4).ceil();
                p.sprite(
                    bubble_key,
                    Rect::new(transcript.x, y - room, transcript.w, known + room * 2.0),
                    false,
                    |p, r| bubble(p, Rect::new(r.x + pad, r.y + room, content_w, 0.0)),
                )
            } else {
                0.0
            }
        } else {
            bubble(p, area)
        };
        // Pictures and clips sit under the bubble, outside it, as tiles (two to a row). Each
        // can take focus; A opens it full screen.
        if !turn.media.is_empty() {
            let gap = pad * 0.6;
            let tile_w = ((content_w - gap) * 0.5).min(m.row * 6.5);
            let mut ty = y + h + if h > 0.0 { gap } else { 0.0 };
            let mut row_h: f32 = 0.0;
            for (k, item) in turn.media.iter().enumerate() {
                let col = (k % 2) as f32;
                if k % 2 == 0 && k > 0 {
                    ty += row_h + gap;
                    row_h = 0.0;
                }
                // The tile takes the picture's shape, within sensible limits.
                let aspect = item
                    .picture
                    .as_deref()
                    .and_then(|path| pics.size(path))
                    .map_or(1.0, |(w, h)| h as f32 / w.max(1) as f32)
                    .clamp(0.5, 1.25);
                let tile = Rect::new(area.x + col * (tile_w + gap), ty, tile_w, tile_w * aspect);
                let id = MEDIA_FOCUS_BASE + media_index;
                media_index += 1;
                // The tile shows a placeholder until its picture is decoded: that is part
                // of what it looks like.
                let ready = item
                    .picture
                    .as_deref()
                    .is_some_and(|x| pics.get(x).is_some());
                let tile_key = sprite_key(&(
                    0x7Eu8,
                    &item.picture,
                    item.is_video,
                    ready,
                    tile.w.to_bits(),
                    tile.h.to_bits(),
                ));
                p.sprite(tile_key, tile, false, |p, r| {
                    media_tile(p, pics, item, r, false);
                    0.0
                });
                if p.ui.focus.is_focused(id) {
                    p.focus_ring(tile, m.radius_card);
                }
                p.focusable(id, tile, 1);
                row_h = row_h.max(tile.h);
            }
            h = ty + row_h - y;
        }
        if let Some(t) = rising {
            // Fade in by covering the fresh bubble with the page colour, thinning as it settles.
            let cover = Rect::new(transcript.x, top, transcript.w, h + lift + pad * 0.5);
            p.canvas.fill_rect(cover, p.theme.pal.page.fade(1.0 - t));
        }
        pics.heights.insert(key, h);
        next_y = top + h + pad * 0.9;
    }
    let mut y = next_y;
    // Muse is working and has not started its reply: the "•••" bubble holds its place.
    let waiting = data.agent_busy()
        && turns
            .last()
            .map_or(true, |t| t.from_user || t.status != TurnStatus::Replying);
    if waiting {
        let area = Rect::new(transcript.x + pad, y, content_w, 0.0);
        y += p.typing_bubble(area) + pad * 0.9;
    }
    // Something is wrong with the connection: say what, and where to look.
    let link = data.status().link;
    if link != LinkState::Connected {
        let hermes = data.agent_kind() == Some(crate::data::AgentKind::Hermes);
        let (title, hint) = match (link, hermes) {
            (LinkState::NotPaired, _) => ("Not paired", "Menu: Settings → Profiles"),
            (LinkState::Reconnecting, false) => ("Reconnecting to Muse…", ""),
            (LinkState::Reconnecting, true) => ("Reconnecting to Hermes…", ""),
            (_, false) => ("Can't reach Muse", "See Status"),
            (_, true) => ("Can't reach Hermes", "See Status"),
        };
        let h = m.row * 1.25;
        let r = Rect::new(transcript.x + pad, y, content_w, h);
        p.canvas
            .fill_rrect(r, m.radius_card * 0.6, p.theme.pal.warn.fade(0.16));
        let ic = m.body * 1.1;
        p.icon(
            Icon::Alert,
            ic,
            r.x + pad * 0.8,
            r.cy() - ic * 0.5,
            p.theme.pal.warn,
        );
        p.line(
            title,
            TextRole::BodyStrong,
            r.x + pad * 1.6 + ic,
            r.y + h * 0.12,
            r.w - pad * 3.0 - ic,
            p.theme.pal.text,
        );
        p.line(
            hint,
            TextRole::Caption,
            r.x + pad * 1.6 + ic,
            r.y + h * 0.54,
            r.w - pad * 3.0 - ic,
            p.theme.pal.text_muted,
        );
        y += h + pad * 0.9;
    }
    let content_bottom = y + scroll;
    end(p, clip);
    // More conversation below the visible part: fade it out, never a hard cut.
    let below = content_bottom - scroll - transcript.bottom();
    if below > 1.0 {
        let fade_h = (m.pad * 2.4).ceil() + 1.0;
        p.sprite(
            sprite_key(&(0xEFu8, transcript.w.to_bits(), layout_key)),
            Rect::new(
                transcript.x,
                transcript.bottom() - fade_h,
                transcript.w,
                fade_h,
            ),
            false,
            |p, r| {
                p.edge_fade(r, true);
                0.0
            },
        );
    }
    // Scrolled well back: a cue that B returns to the newest message.
    if below > transcript.h * 0.5 && !keyboard_open {
        let kh = m.caption * 1.5;
        let label = "Newest";
        let w = kh + p.measure(label, TextRole::Caption) + pad * 1.7;
        let pill = Rect::new(
            transcript.right() - pad - w,
            transcript.bottom() - kh * 1.6 - pad * 0.4,
            w,
            kh * 1.6,
        );
        p.sprite(
            sprite_key(&(0x4Eu8, layout_key, w.to_bits())),
            pill.inflate(2.0),
            false,
            |p, r| {
                let pill = r.inflate(-2.0);
                p.canvas.fill_rrect(
                    pill.inset(-1.0, -1.0),
                    pill.h * 0.5 + 1.0,
                    p.theme.pal.subtle_strong,
                );
                p.canvas.fill_rrect(pill, pill.h * 0.5, p.theme.pal.surface);
                let kw = p.keycap(Button::B, pill.x + pad * 0.5, pill.cy(), kh);
                let tx = pill.x + pad * 0.5 + kw + pad * 0.35;
                p.line_centered(
                    label,
                    TextRole::Caption,
                    Rect::new(tx, pill.y, pill.right() - tx - pad * 0.4, pill.h),
                    p.theme.pal.text,
                );
                0.0
            },
        );
    }

    if let (false, Some((elapsed_ms, cap_ms, level))) = (keyboard_open, recorder) {
        // Recording a voice note: the composer is the recorder (no reply choices meanwhile).
        let strip = Rect::new(body.x, transcript.bottom(), body.w, composer_h);
        let (_, composer_row) = strip.inset(pad, pad * 0.5).split_top(m.row * 0.9);
        let bar = Rect::new(
            composer_row.x,
            composer_row.y + pad * 0.3,
            composer_row.w,
            m.row,
        );
        p.recorder_bar(bar, elapsed_ms, cap_ms, level);
    } else if !keyboard_open {
        if let Some((item, fit)) = request {
            let (head, facts) = (&item.heading, &item.facts);
            let key = sprite_key(&(
                0xA5u8, head, facts, &item.body, layout_key, fit.lines, fit.whole,
            ));
            let [head_n, body_n, facts_n] = fit.lines;
            let [head_line, line] = fit.per_line;
            p.sprite(key, fit.rect.inflate(2.0), false, |p, r| {
                let card = r.inflate(-2.0);
                let pal = p.theme.pal;
                p.canvas.fill_rrect(card, m.radius_card * 0.8, pal.surface);
                p.canvas
                    .stroke_rrect(card, m.radius_card * 0.8, m.hairline * 2.0, pal.accent);
                let isz = m.body * 1.1;
                let (head_w, body_w) = card_columns(&m, card.w);
                let mut y = card.y + pad * 0.6;
                p.icon(Icon::Shield, isz, card.x + pad * 0.8, y, pal.accent);
                let x = card.x + pad * 0.8 + isz + pad * 0.5;
                // Every line there is room for, and none cut short: what is left off is
                // left off whole, and the card says so in its last line.
                let strong = TextRole::BodyStrong;
                p.paragraph_lines(head, strong, x, y - m.body * 0.1, head_w, pal.text, head_n);
                y += m.body * 1.5 + (head_n as f32 - 1.0) * head_line;
                let small = TextRole::Caption;
                p.paragraph_lines(&item.body, small, x, y, body_w, pal.text, body_n);
                y += body_n as f32 * line + m.caption * 0.5;
                p.paragraph_lines(facts, small, x, y, head_w, pal.text_muted, facts_n);
                if !fit.whole {
                    y += facts_n as f32 * line;
                    p.line(NOT_WHOLE, small, x, y, head_w, pal.text_muted);
                }
                0.0
            });
        }
        let strip = Rect::new(body.x, transcript.bottom() + card_h, body.w, strip_h);
        // The reply choices and the composer: one piece, redrawn when what it shows or
        // what is focused in it changes. Its focus targets are registered every frame.
        let strip_key = strip_key(
            composer,
            quick,
            request.map(|(item, _)| item),
            p.ui.focus.current(),
            data.call().is_some(),
            layout_key,
        );
        p.sprite(strip_key, strip, true, |p, strip| {
            // Quick-reply chips row.
            let (chips_row, composer_row) = strip.inset(pad, pad * 0.5).split_top(m.row * 0.9);
            let mut cx = chips_row.x;
            let chip_h = m.row * 0.72;
            for (i, q) in quick.iter().enumerate() {
                // The answers to a request carry the button that gives them.
                let key = request
                    .and_then(|(r, _)| r.answers.get(i))
                    .and_then(|a| answer_button(&a.id));
                let kh = chip_h * 0.74;
                let kw = key.map_or(0.0, |b| p.keycap_width(b, kh) + pad * 0.4);
                let label_w = p.measure(q, TextRole::Body);
                let w = chip_width(p, q, key);
                if cx + w > chips_row.right() {
                    break;
                }
                let r = Rect::new(cx, chips_row.cy() - chip_h * 0.5, w, chip_h);
                let id = 100 + i as u64;
                let focused = p.ui.focus.is_focused(id);
                match key {
                    Some(button) => {
                        p.chip(r, "", false, focused);
                        p.keycap(button, r.x + pad * 0.6, r.cy(), kh);
                        p.line_centered(
                            q,
                            TextRole::Body,
                            Rect::new(r.x + pad * 0.6 + kw, r.y, label_w + pad * 0.4, r.h),
                            p.theme.pal.text,
                        );
                    }
                    None => {
                        p.chip(r, q, false, focused);
                    }
                }
                p.focusable(id, r, 2);
                cx += w + pad * 0.5;
            }
            // Composer pill.
            let comp = Rect::new(
                composer_row.x,
                composer_row.y + pad * 0.3,
                composer_row.w,
                m.row,
            );
            let focused = p.ui.focus.is_focused(1);
            p.canvas.fill_rrect(comp, comp.h * 0.5, p.theme.pal.subtle);
            // The composer names its real controls: X types, F1 (hold) talks, Start sends a draft.
            // (Android's "+" and mic are touch targets; here they are buttons.)
            let ic = m.body * 1.1;
            let kh = comp.h * 0.56;
            let kx = comp.x + pad * 0.7;
            let kw = p.keycap(Button::X, kx, comp.cy(), kh);
            let tx = kx + kw + pad * 0.6;
            // Right side: Start + send when there is a draft, otherwise F1 + mic.
            let right_w = if composer.is_empty() && data.call().is_some() {
                // On a call F1 mutes (shown with the call's controls), so no voice note here.
                pad
            } else if composer.is_empty() {
                let fw = p.keycap_width(Button::R1, kh);
                let rx = comp.right() - pad * 0.7 - ic;
                p.icon(
                    Icon::Mic,
                    ic,
                    rx,
                    comp.cy() - ic * 0.5,
                    p.theme.pal.text_muted,
                );
                p.keycap(Button::R1, rx - pad * 0.4 - fw, comp.cy(), kh);
                ic + fw + pad * 1.5
            } else {
                let sw = p.keycap_width(Button::Start, kh);
                let rx = comp.right() - pad * 0.7 - ic;
                p.icon(Icon::Send, ic, rx, comp.cy() - ic * 0.5, p.theme.pal.accent);
                p.keycap(Button::Start, rx - pad * 0.4 - sw, comp.cy(), kh);
                ic + sw + pad * 1.5
            };
            let text_w = comp.right() - right_w - tx;
            if composer.is_empty() {
                p.line(
                    "Message",
                    TextRole::Body,
                    tx,
                    comp.cy() - m.body * 0.65,
                    text_w,
                    p.theme.pal.text_muted,
                );
            } else {
                p.line(
                    composer,
                    TextRole::Body,
                    tx,
                    comp.cy() - m.body * 0.65,
                    text_w,
                    p.theme.pal.text,
                );
            }
            if focused {
                p.focus_ring(comp, comp.h * 0.5);
            }
            p.focusable(1, comp, 2);
            0.0
        });
    }

    // The caller clamps scroll against the whole body, but only `transcript` scrolls; add the
    // reserved composer strip so the last message can scroll fully into view.
    (content_bottom - transcript.y).max(transcript.h) + composer_h + header_h
}

/// A gap this long between messages starts a new group with its own time label.
pub const GROUP_GAP_MS: u64 = 15 * 60 * 1000;

/// The wall clock in Unix milliseconds.
fn wall_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn local_tm(ms: u64) -> Option<libc::tm> {
    let t = (ms / 1000) as libc::time_t;
    // SAFETY: localtime_r writes only into `tm`, which we own.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    (!unsafe { libc::localtime_r(&t, &mut tm) }.is_null()).then_some(tm)
}

/// A message group's time as the Muse app words it: "7:42 PM" today, "Yesterday 7:42 PM",
/// else "Nov 10, 7:42 PM" (local time).
pub fn time_label(at_ms: u64, now_ms: u64) -> String {
    let (Some(at), Some(now)) = (local_tm(at_ms), local_tm(now_ms)) else {
        return String::new();
    };
    time_label_from(
        (
            at.tm_year, at.tm_yday, at.tm_mon, at.tm_mday, at.tm_hour, at.tm_min,
        ),
        (now.tm_year, now.tm_yday),
        now_ms.saturating_sub(at_ms),
    )
}

/// [`time_label`] from broken-down local times: `at` is (year, day of year, month 0-11, day
/// of month, hour, minute), `now` is (year, day of year).
pub fn time_label_from(at: (i32, i32, i32, i32, i32, i32), now: (i32, i32), age_ms: u64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (year, yday, mon, mday, hour, min) = at;
    let clock = format!(
        "{}:{min:02} {}",
        match hour % 12 {
            0 => 12,
            h => h,
        },
        if hour < 12 { "AM" } else { "PM" }
    );
    let two_days = 48 * 3600 * 1000;
    if (year, yday) == now {
        clock
    } else if age_ms < two_days
        && ((year == now.0 && yday + 1 == now.1)
            || (year + 1 == now.0 && now.1 == 0 && mon == 11 && mday == 31))
    {
        format!("Yesterday {clock}")
    } else {
        format!("{} {mday}, {clock}", MONTHS[mon.clamp(0, 11) as usize])
    }
}

/// What a chat message's height depends on, as one number: its text, state, reaction and
/// media, and the layout it is drawn in (`layout`).
fn turn_key(turn: &crate::data::Turn, layout: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    layout.hash(&mut h);
    turn.from_user.hash(&mut h);
    turn.body.hash(&mut h);
    std::mem::discriminant(&turn.status).hash(&mut h);
    turn.reaction.hash(&mut h);
    for m in &turn.media {
        m.picture.hash(&mut h);
        m.is_video.hash(&mut h);
    }
    h.finish()
}

/// Focus ids of media tiles in the chat start here, in the order the media appear.
pub const MEDIA_FOCUS_BASE: u64 = 3000;

/// One picture or clip as a rounded tile: the picture when it is on the device, a quiet
/// placeholder until then, and a play mark for video.
pub fn media_tile(
    p: &mut Painter,
    pics: &mut crate::media::PictureCache,
    item: &crate::media::Media,
    tile: Rect,
    focused: bool,
) {
    let m = p.m();
    let radius = m.radius_card;
    // Off screen (drawn only to be measured): nothing to show, and above all no picture
    // to decode.
    let clip = p.canvas.clip();
    if tile.bottom() < clip.y as f32 || tile.y > clip.bottom() as f32 {
        return;
    }
    p.canvas.fill_rrect(tile, radius, p.theme.pal.subtle_strong);
    match item.picture.as_deref().and_then(|path| pics.get(path)) {
        Some(img) => p.canvas.draw_image(img, tile, radius, 1.0),
        None => {
            let ic = m.body * 1.6;
            let icon = if item.is_video {
                Icon::File
            } else {
                Icon::Image
            };
            p.icon_centered(icon, ic, tile, p.theme.pal.text_muted);
        }
    }
    if item.is_video {
        // A play mark: a dark disc with a light triangle (drawn as a chevron).
        let d = (tile.w.min(tile.h) * 0.28).max(m.row * 0.7);
        p.canvas
            .fill_circle(tile.cx(), tile.cy(), d * 0.5, Color::BLACK.fade(0.55));
        let disc = Rect::new(tile.cx() - d * 0.5, tile.cy() - d * 0.5, d, d);
        p.icon_centered(Icon::ChevronRight, d * 0.7, disc, Color::WHITE);
    }
    if focused {
        p.focus_ring(tile, radius);
    }
}

/// Settings root.
/// What a profile's row says about it.
pub fn profile_state(p: &crate::data::ProfileEntry) -> &'static str {
    use crate::data::AgentKind::{Hermes, Muse};
    match (p.active, p.paired, p.agent) {
        (true, true, Some(Muse)) => "In use · Muse",
        (true, true, Some(Hermes)) => "In use · Hermes",
        (true, true, None) => "In use",
        (true, false, _) => "In use · not paired",
        (false, true, Some(Muse)) => "Muse",
        (false, true, Some(Hermes)) => "Hermes",
        (false, true, None) => "Paired",
        (false, false, _) => "Not paired",
    }
}

/// How the reach of the file commands is named.
pub fn file_scope_label(scope: goblin_ipc::types::FileScope) -> &'static str {
    match scope {
        goblin_ipc::types::FileScope::Home => "Own folder",
        goblin_ipc::types::FileScope::Storage => "SD card",
        goblin_ipc::types::FileScope::Device => "Everything",
    }
}

/// The pages of Settings, switched with L1 / R1. Each fits one screen at the normal size, so
/// nothing is found by scrolling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsTab {
    /// The ways out, and the handheld and its connection. First: Menu opens here.
    Device,
    /// How the app looks.
    Display,
    /// What draws attention: lights, vibration, speech.
    Alerts,
    /// The accounts on this handheld, and when it connects.
    Profiles,
    /// What Muse may do here as the profile in use.
    Access,
}

impl SettingsTab {
    /// The pages in order.
    pub const ALL: [SettingsTab; 5] = [
        SettingsTab::Device,
        SettingsTab::Display,
        SettingsTab::Alerts,
        SettingsTab::Profiles,
        SettingsTab::Access,
    ];

    /// The page's tab.
    pub fn item(self) -> goblin_ui_kit::widgets::TabItem {
        let (icon, label) = match self {
            SettingsTab::Display => (Icon::Palette, "Display"),
            SettingsTab::Alerts => (Icon::Bell, "Alerts"),
            SettingsTab::Profiles => (Icon::User, "Profiles"),
            SettingsTab::Access => (Icon::Shield, "Access"),
            SettingsTab::Device => (Icon::Device, "Device"),
        };
        goblin_ui_kit::widgets::TabItem { icon, label }
    }

    /// The page a row is on.
    pub fn of(row: u64) -> SettingsTab {
        use setting_id as id;
        if row == id::LEAVE_KEEP || row == id::LEAVE_STOP {
            SettingsTab::Device
        } else if row >= id::BUILD_BASE || row == id::BUILDS {
            SettingsTab::Access
        } else if row >= id::PROFILE_BASE || [id::NEW_PROFILE, id::AUTOSTART].contains(&row) {
            SettingsTab::Profiles
        } else if row == id::FILES || id::CONSENT.contains(&row) {
            SettingsTab::Access
        } else if [
            id::LIGHTS,
            id::VIBRATE,
            id::VOICE_TO_VOICE,
            id::CALL_GREETING,
        ]
        .contains(&row)
        {
            SettingsTab::Alerts
        } else {
            SettingsTab::Display
        }
    }
}

/// How the builds switch is named.
pub fn build_access_label(access: goblin_ipc::types::BuildAccess) -> &'static str {
    match access {
        goblin_ipc::types::BuildAccess::Off => "No",
        goblin_ipc::types::BuildAccess::Ask => "Ask first",
        goblin_ipc::types::BuildAccess::Free => "Freely",
    }
}

/// What a built item's row says about it.
pub fn build_state_label(item: &goblin_ipc::types::BuildItem) -> &'static str {
    use goblin_ipc::types::{BuildKind, BuildState};
    match (item.state, item.kind) {
        (BuildState::Pending, _) => "Waiting for you",
        (_, BuildKind::Tool) => "Tool",
        (_, BuildKind::App) => "App",
        (_, BuildKind::Game) => "Game",
    }
}

pub fn settings(
    p: &mut Painter,
    snap: &SettingsSnapshot,
    tab: SettingsTab,
    body: Rect,
    scroll: f32,
) -> f32 {
    use setting_id as id;
    let active = snap.profiles.iter().find(|p| p.active);
    let access_title = match active {
        Some(p) => format!("AS \u{201c}{}\u{201d}", p.name.to_uppercase()),
        None => "ACCESS".to_owned(),
    };
    let m = p.m();
    let pad = m.pad;
    let clip = begin(p, body);
    let mut y = body.y + pad * 0.4 - scroll;
    let w = body.w - pad * 2.0;
    let rowh = m.row;
    let mut rows: Vec<(u64, SettingRow)> = match tab {
        SettingsTab::Display => vec![
            (0, SettingRow::section("TEXT AND COLOUR")),
            (id::SIZE, SettingRow::cycle("Size", snap.size)),
            (id::MODE, SettingRow::cycle("Mode", snap.mode)),
            (id::CHAT_THEME, SettingRow::cycle("Colour", snap.chat_theme)),
            (
                id::REDUCE_MOTION,
                SettingRow::toggle("Reduce motion", snap.reduce_motion),
            ),
            (0, SettingRow::section("AVATAR")),
            (id::AVATAR, SettingRow::cycle("Avatar size", snap.avatar)),
        ],
        SettingsTab::Alerts => {
            let mut rows = vec![
                (0, SettingRow::section("WHEN CLOSED")),
                (id::LIGHTS, SettingRow::toggle("Blink lights", snap.lights)),
                (id::VIBRATE, SettingRow::toggle("Vibrate", snap.vibrate)),
                (0, SettingRow::section("SPEECH")),
                (
                    id::VOICE_TO_VOICE,
                    SettingRow::toggle("Speak replies to voice notes", snap.voice_to_voice),
                ),
            ];
            // Only what the agent in use can do is offered.
            if !snap.hermes {
                rows.push((
                    id::CALL_GREETING,
                    SettingRow::toggle("Greet on calls", snap.call_greeting),
                ));
            }
            rows
        }
        SettingsTab::Profiles => {
            let mut rows = vec![(0, SettingRow::section("PROFILES"))];
            for (i, p) in snap.profiles.iter().enumerate() {
                rows.push((
                    id::PROFILE_BASE + i as u64,
                    SettingRow::value(&p.name, profile_state(p)),
                ));
            }
            rows.push((
                id::NEW_PROFILE,
                SettingRow::value("New profile\u{2026}", ""),
            ));
            rows.push((0, SettingRow::section("CONNECTION")));
            if let Some(on) = snap.autostart {
                rows.push((
                    id::AUTOSTART,
                    SettingRow::toggle("Connect when the handheld starts", on),
                ));
            }
            rows
        }
        // Lines about the handheld are added below, from the snapshot.
        SettingsTab::Device => vec![
            (
                id::LEAVE_KEEP,
                SettingRow::value("Leave, stay connected", ""),
            ),
            (
                id::LEAVE_STOP,
                SettingRow::value("Leave and disconnect", ""),
            ),
            (0, SettingRow::section("DEVICE")),
        ],
        // From what reaches least to what reaches most.
        SettingsTab::Access => vec![
            (0, SettingRow::section(&access_title)),
            (
                id::CONSENT[1],
                SettingRow::toggle("Read files", snap.consent.file_read),
            ),
            (
                id::CONSENT[2],
                SettingRow::toggle("Write files", snap.consent.file_write),
            ),
            (
                id::FILES,
                SettingRow::cycle("File reach", file_scope_label(snap.consent.files)),
            ),
            (
                id::CONSENT[4],
                SettingRow::toggle(
                    "Write outside own folder",
                    snap.consent.file_write_outside_home,
                ),
            ),
            (
                id::CONSENT[3],
                SettingRow::toggle("Commands you added", snap.consent.drop_ins)
                    .with_detail("Command files you put on the card. None are built in"),
            ),
            (
                id::CONSENT[0],
                SettingRow::toggle("Run shell commands", snap.consent.shell)
                    .with_detail("Anything at all, on the whole handheld and every profile"),
            ),
            (
                id::BUILDS,
                SettingRow::cycle(
                    "Install its builds",
                    build_access_label(snap.consent.builds),
                )
                .with_detail("Run without limits once installed"),
            ),
        ],
    };
    if tab == SettingsTab::Device {
        for (label, value) in &snap.device {
            let mut row = SettingRow::value(label, value);
            row.chevron = false;
            rows.push((0, row));
        }
    }
    if tab == SettingsTab::Access && !snap.builds.is_empty() {
        rows.push((
            0,
            SettingRow::section(if snap.hermes {
                "BUILT BY HERMES"
            } else {
                "BUILT BY MUSE"
            }),
        ));
        for (i, b) in snap.builds.iter().enumerate() {
            rows.push((
                id::BUILD_BASE + i as u64,
                SettingRow::value(&b.title, build_state_label(b)),
            ));
        }
    }
    for (rid, row) in rows {
        let h = if row.kind == RowKind::Section {
            m.body * 1.9
        } else if row.detail.is_empty() {
            rowh
        } else {
            rowh * 1.3
        };
        let r = Rect::new(body.x + pad, y, w, h);
        let focused = rid != 0 && p.ui.focus.is_focused(rid);
        p.setting_row(r, &row, focused);
        if rid != 0 {
            p.focusable(rid, r, 1);
        }
        y += h;
    }
    y += pad;
    end(p, clip);
    (y + scroll - body.y).max(body.h)
}

#[cfg(test)]
mod card_tests {
    use super::{answers_offered, card_layout, CardLayout};
    use goblin_ipc::types::{RequestAnswer, RequestKind, RequestView};
    use goblin_ui_kit::widgets::Painter;
    use goblin_ui_kit::{Canvas, Mode, Rect, Theme, Ui};

    fn request(kind: RequestKind, body: &str, answers: &[&str]) -> RequestView {
        RequestView {
            id: "prompt:p1".into(),
            heading: "Run this command?".into(),
            body: body.into(),
            facts: String::new(),
            chat: "main".into(),
            kind,
            cut: false,
            answers: answers
                .iter()
                .map(|id| RequestAnswer {
                    id: (*id).into(),
                    label: "Go".into(),
                })
                .collect(),
        }
    }

    /// The card on a Brick's screen, under a header `top_inset` deep.
    fn laid_out(ui: &mut Ui, item: &RequestView, top_inset: f32) -> CardLayout {
        let theme = Theme::new(Mode::Dark, 2.0);
        let mut canvas = Canvas::new(1024, 768);
        let mut p = Painter::new(&mut canvas, ui, &theme, 0);
        card_layout(&mut p, item, Rect::new(0.0, 0.0, 1024.0, 728.0), top_inset)
    }

    fn offered(item: &RequestView, card: Option<&CardLayout>) -> Vec<String> {
        answers_offered(item, card)
            .into_iter()
            .map(|a| a.id)
            .collect()
    }

    #[test]
    fn only_what_the_card_showed_is_offered() {
        let ui = &mut Ui::new().unwrap();
        let approval = request(RequestKind::Approval, "ls", &["yes", "hour", "no"]);
        let whole = laid_out(ui, &approval, 120.0);
        assert!(whole.whole && whole.answers == 3, "{whole:?}");
        assert_eq!(offered(&approval, Some(&whole)), ["yes", "hour", "no"]);
        // Not drawn yet, or drawn with something left off: an approval can only be refused.
        let cut = CardLayout {
            whole: false,
            ..whole
        };
        assert_eq!(offered(&approval, None), ["no"]);
        assert_eq!(offered(&approval, Some(&cut)), ["no"]);
        // One that offers no "no" then offers nothing.
        let yes_only = request(RequestKind::Approval, "ls", &["yes"]);
        assert!(offered(&yes_only, Some(&cut)).is_empty());
        // A question keeps the answers that have a button, cut or not, and none before
        // it is drawn.
        let question = request(RequestKind::Question, "ls", &["yes", "no", "hour"]);
        assert_eq!(offered(&question, Some(&cut)), ["yes", "no", "hour"]);
        let two = CardLayout { answers: 2, ..cut };
        assert_eq!(offered(&question, Some(&two)), ["yes", "no"]);
        assert!(offered(&question, None).is_empty());
        // An approval whose buttons do not all fit is not whole either.
        assert_eq!(offered(&approval, Some(&two)), ["no"]);
    }

    #[test]
    fn a_card_is_whole_only_with_every_word_on_it() {
        let ui = &mut Ui::new().unwrap();
        let lines = |n: usize| {
            let body = (0..n).map(|i| format!("echo {i}")).collect::<Vec<_>>();
            request(RequestKind::Approval, &body.join("\n"), &["yes", "no"])
        };
        // As many lines as there is room for, and then one more.
        let most = (1..200)
            .take_while(|n| laid_out(ui, &lines(*n), 120.0).whole)
            .last()
            .expect("a short command fits");
        assert!(most >= 8, "{most} lines fit");
        let (fits, over) = (
            laid_out(ui, &lines(most), 120.0),
            laid_out(ui, &lines(most + 1), 120.0),
        );
        assert_eq!(fits.lines, [1, most, 0]);
        assert!(!over.whole);
        // The one that is over shows whole lines up to the note that says so, and stays
        // in the same room.
        assert!(over.lines[1] < most + 1, "{over:?}");
        for card in [fits, over] {
            assert!(card.rect.y >= card.ceiling, "{card:?}");
            assert!((card.ceiling - 130.0).abs() < 0.01, "{card:?}");
        }
        // A long word is broken, not run off the edge: all of it counts as lines.
        let blob = request(RequestKind::Approval, &"A1b2".repeat(100), &["yes", "no"]);
        let card = laid_out(ui, &blob, 120.0);
        assert!(card.whole && card.lines[1] > 1, "{card:?}");
        // What the handheld cut on arrival is never whole, however short.
        let mut cut = lines(1);
        cut.cut = true;
        assert!(!laid_out(ui, &cut, 120.0).whole);
        // Nor is text with a character no font here can draw (it would be a box).
        let unknown = request(RequestKind::Approval, "ls \u{378}", &["yes", "no"]);
        assert!(!laid_out(ui, &unknown, 120.0).whole);
        let mut heading = lines(1);
        heading.heading = "Run \u{378}?".into();
        assert!(!laid_out(ui, &heading, 120.0).whole);
        // A heading too long for one line takes more, and is whole while they fit.
        let mut long = lines(1);
        long.heading = ["Run this command, which has a long name?"; 4].join(" ");
        let card = laid_out(ui, &long, 120.0);
        assert!(card.whole && card.lines[0] > 1, "{card:?}");
        // Answers with no room for their buttons leave the card not whole.
        let mut wide = lines(1);
        for a in wide.answers.iter_mut() {
            a.label = "An answer of some length".repeat(2);
        }
        let card = laid_out(ui, &wide, 120.0);
        assert!(!card.whole && card.answers < 2, "{card:?}");
        // With next to no room there is still the heading's first line and the note.
        let squeezed = laid_out(ui, &lines(most), 560.0);
        assert_eq!(squeezed.lines, [1, 0, 0]);
        assert!(!squeezed.whole && squeezed.rect.y > 0.0, "{squeezed:?}");
    }
}

#[cfg(test)]
mod shown_tests {
    use super::{answers_offered, card_layout, strip_key};
    use goblin_ipc::types::{RequestAnswer, RequestKind, RequestView};
    use goblin_ui_kit::widgets::Painter;
    use goblin_ui_kit::{Canvas, Mode, Rect, Theme, Ui};

    fn approval(body: &str, answers: &[(&str, &str)]) -> RequestView {
        RequestView {
            id: "prompt:p1".into(),
            heading: "Run this command?".into(),
            body: body.into(),
            facts: String::new(),
            chat: "main".into(),
            kind: RequestKind::Approval,
            cut: false,
            answers: answers
                .iter()
                .map(|(id, label)| RequestAnswer {
                    id: (*id).into(),
                    label: (*label).into(),
                })
                .collect(),
        }
    }

    /// The answers a card on a Brick offers for `item`, and whether it is whole.
    fn offered(ui: &mut Ui, item: &RequestView) -> (bool, Vec<String>) {
        let theme = Theme::new(Mode::Dark, 2.0);
        let mut canvas = Canvas::new(1024, 768);
        let mut p = Painter::new(&mut canvas, ui, &theme, 0);
        let card = card_layout(&mut p, item, Rect::new(0.0, 0.0, 1024.0, 728.0), 120.0);
        let ids = answers_offered(item, Some(&card))
            .into_iter()
            .map(|a| a.id)
            .collect();
        (card.whole, ids)
    }

    /// A character that is in the command and not on the screen leaves the card not
    /// whole, however short the command: it does not read as it will run. The owner can
    /// then only refuse.
    #[test]
    fn a_command_with_a_character_the_screen_does_not_show_can_only_be_refused() {
        let ui = &mut Ui::new().unwrap();
        let answers = [("yes", "Run once"), ("hour", "Session"), ("no", "No")];
        for (what, command) in [
            // Reads as a test that is always true. The joiner after the second `a` makes
            // it false, and the `rm` runs.
            ("a joiner", "[ a = a\u{34f} ] || rm -rf ~/Documents"),
            ("a tag character", "echo ok\u{e0041}"),
            ("a tag space", "rm -rf build\u{e0020}/"),
            ("a no-break space", "rm -rf ~/old\u{a0}files"),
            ("an em space", "ls\u{2003}-la"),
            ("a thin space", "ls\u{2009}-la"),
            ("a narrow no-break space", "ls\u{202f}-la"),
            ("a zero width space", "ls\u{200b} -la"),
            ("a soft hyphen", "ls \u{ad}-la"),
            ("a word joiner", "ls\u{2060} -la"),
            // The two ticks are drawn alike, and only one has a selector after it: the
            // test is false, and the rm runs.
            (
                "a selector after a symbol",
                "[ \u{2713} = \u{2713}\u{fe0f} ] || rm -rf ~/Documents",
            ),
            ("a selector after a warning sign", "\u{26a0}\u{fe0f} ls"),
            ("a text selector after a symbol", "ls \u{2764}\u{fe0e}"),
            ("a selector after a letter", "ls a\u{fe0f}"),
            ("a selector after a bracket", "[ a = a ]\u{fe0f}"),
            ("a selector after nothing", "\u{fe0f}ls"),
            ("a selector after a selector", "\u{26a0}\u{fe0f}\u{fe0f} ls"),
            ("another kind of selector", "ls a\u{fe00}"),
            ("a blank braille cell", "ls\u{2800}-la"),
            ("an accent with no letter before it", "ls \u{301}a"),
            ("an accent first", "\u{301}ls"),
            ("an accent first on its line", "ls\n\u{338}= a"),
            ("a tab", "ls\t-la"),
            ("a carriage return", "ls\r\n-la"),
            ("a character with no glyph here", "ls \u{4e2d}\u{6587}"),
        ] {
            let (whole, ids) = offered(ui, &approval(command, &answers));
            assert!(!whole, "{what}");
            assert_eq!(ids, ["no"], "{what}");
        }
        // In the heading or the facts it is the same.
        let mut item = approval("ls", &answers);
        item.heading = "Run this\u{a0}command?".into();
        assert_eq!(offered(ui, &item), (false, vec!["no".to_owned()]));
        let mut item = approval("ls", &answers);
        item.facts = "Runs bin/a.sh\u{34f}".into();
        assert_eq!(offered(ui, &item), (false, vec!["no".to_owned()]));
        // What is ordinary stays whole: letters with accents, dashes, quotes, an indented
        // line, and the emoji Hermes opens an approval with, selector and all.
        for (what, command) in [
            ("plain", "rm -rf build && make all"),
            (
                "accents and dashes",
                "echo \"caf\u{e9} \u{2014} d\u{e9}j\u{e0}\" | tee 'a b'",
            ),
            ("indentation", "if ready:\n    run()\nelse:\n    wait()"),
            ("a combining accent", "echo cafe\u{301}"),
            ("symbols", "\u{26a0} ls \u{2705} \u{2713} \u{2192}"),
        ] {
            let all: Vec<String> = answers.iter().map(|(id, _)| (*id).to_owned()).collect();
            assert_eq!(
                offered(ui, &approval(command, &answers)),
                (true, all),
                "{what}"
            );
        }
        // The words on an answer are read like the rest: with a character in them that
        // is not seen, the card is not whole.
        let sly = [
            ("yes", "Run on\u{34f}ce"),
            ("hour", "Session"),
            ("no", "No"),
        ];
        assert_eq!(
            offered(ui, &approval("ls", &sly)),
            (false, vec!["no".to_owned()])
        );
    }

    /// Spaces are not drawn, so where they change what a command means and nothing on the
    /// card stands for them, the card is not whole: at the end of a line (in sh a
    /// backslash, a space and a line break is not a continued line), several where a line
    /// is carried over (they would read as one), and indentation wider than the card.
    #[test]
    fn spaces_a_card_cannot_stand_for_leave_it_not_whole() {
        let ui = &mut Ui::new().unwrap();
        let answers = [("yes", "Run once"), ("no", "No")];
        let no = (false, vec!["no".to_owned()]);
        let all = (true, vec!["yes".to_owned(), "no".to_owned()]);
        // The second reads as one command with --dry-run, and runs without it.
        let continued = "cmd --force \\\n  --dry-run";
        let not_continued = "cmd --force \\ \n  --dry-run";
        assert_eq!(offered(ui, &approval(continued, &answers)), all);
        assert_eq!(offered(ui, &approval(not_continued, &answers)), no);
        assert_eq!(offered(ui, &approval("ls -la ", &answers)), no);
        assert_eq!(offered(ui, &approval("ls\n    \nls", &answers)), no);
        // Wherever this is carried over, it is at five spaces.
        let gaps = ["ab"; 200].join("     ");
        assert_eq!(offered(ui, &approval(&gaps, &answers)), no);
        // On one line the same gaps are drawn as wide as they are.
        assert_eq!(offered(ui, &approval("ab     ab     ab", &answers)), all);
        // Carried over at single spaces, nothing is lost.
        let words = ["ab"; 200].join(" ");
        assert_eq!(offered(ui, &approval(&words, &answers)), all);
        let deep = format!("{}x", " ".repeat(400));
        assert_eq!(offered(ui, &approval(&deep, &answers)), no);
        // In the heading or the facts it is the same.
        let mut item = approval("ls", &answers);
        item.heading = "Run this command? ".into();
        assert_eq!(offered(ui, &item), no);
        let mut item = approval("ls", &answers);
        item.facts = "Runs bin/a.sh ".into();
        assert_eq!(offered(ui, &item), no);
    }

    /// The strip of answers is drawn once and placed after. What it shows is each
    /// answer's words on that answer's own button, so two requests with the same words on
    /// other buttons are two strips: a strip kept from the first must not be shown for
    /// the second, where B, A and Y mean something else.
    #[test]
    fn answers_with_the_same_words_on_other_buttons_are_another_strip() {
        let labels = ["Allow Once", "Allow Session", "Deny"];
        // A question's choices take A, B and Y as they come; an approval's no is B.
        let question = approval(
            "ls",
            &[("yes", labels[0]), ("no", labels[1]), ("hour", labels[2])],
        );
        let asked = approval(
            "ls",
            &[("yes", labels[0]), ("hour", labels[1]), ("no", labels[2])],
        );
        let key = |item: Option<&RequestView>| strip_key("", &labels, item, Some(100), false, 7);
        assert_ne!(key(Some(&question)), key(Some(&asked)));
        assert_ne!(key(Some(&asked)), key(None), "replies, not answers");
        assert_eq!(key(Some(&asked)), key(Some(&asked.clone())));
        // The rest of what it shows still tells strips apart.
        assert_ne!(
            key(Some(&asked)),
            strip_key("draft", &labels, Some(&asked), Some(100), false, 7)
        );
        assert_ne!(
            key(Some(&asked)),
            strip_key("", &labels, Some(&asked), Some(101), false, 7)
        );
    }
}

#[cfg(test)]
mod time_label_tests {
    use super::time_label_from;

    #[test]
    fn today_yesterday_and_older() {
        let hour = 3_600_000;
        // (year, yday, month, mday, hour, minute)
        assert_eq!(
            time_label_from((126, 315, 10, 12, 19, 42), (126, 315), hour),
            "7:42 PM"
        );
        assert_eq!(
            time_label_from((126, 315, 10, 12, 0, 5), (126, 315), hour),
            "12:05 AM"
        );
        assert_eq!(
            time_label_from((126, 314, 10, 11, 9, 0), (126, 315), 20 * hour),
            "Yesterday 9:00 AM"
        );
        assert_eq!(
            time_label_from((126, 363, 11, 30, 11, 0), (127, 0), 47 * hour),
            "Dec 30, 11:00 AM"
        );
        assert_eq!(
            time_label_from((126, 364, 11, 31, 23, 30), (127, 0), 2 * hour),
            "Yesterday 11:30 PM"
        );
        assert_eq!(
            time_label_from((126, 300, 9, 28, 12, 0), (126, 315), 400 * hour),
            "Oct 28, 12:00 PM"
        );
    }
}
