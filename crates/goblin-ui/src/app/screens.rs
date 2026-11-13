//! Screen draw functions. Each takes a [`Painter`], its data, the body rectangle and a scroll
//! offset, draws the screen (registering focusables at their on-screen positions) and returns the
//! total content height so the router can clamp scrolling.

use goblin_ui_kit::widgets::{
    sprite_key, AgentPanel, Bubble, LinkState, ListRow, Painter, RowKind, SettingRow, TextRole,
    TurnStatus,
};
use goblin_ui_kit::{AvatarSize, Button, Color, Icon, Mode, Rect, Theme};

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
    /// Opens the button test.
    pub const INPUT_TEST: u64 = 32;
    /// Consent: shell, file read, file write, drop-ins.
    pub const CONSENT: [u64; 4] = [12, 13, 16, 17];
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
    request: Option<&goblin_ipc::types::RequestView>,
    top_inset: f32,
    recorder: Option<(u64, f32)>,
    appear: Option<(usize, f32)>,
    pics: &mut crate::media::PictureCache,
) -> f32 {
    let m = p.m();
    let pad = m.pad;
    // Reserve the composer + chips strip at the bottom (hidden while the keyboard is open, which
    // draws its own area).
    let strip_h = if keyboard_open {
        0.0
    } else {
        m.row + m.row * 0.9 + pad * 2.0
    };
    // A request from Muse sits above the answer buttons until it is answered. It is drawn
    // by the handheld from what is to be installed: nothing Muse writes can look like it.
    let request = request.filter(|_| !keyboard_open);
    let card_w = body.w - pad * 2.0;
    let card_inner = card_w - pad * 2.4;
    let card_h = match request {
        Some(item) => {
            pad * 1.4
                + m.body * 1.5
                + p.paragraph_height(&item.body, TextRole::Caption, card_inner)
                + if item.facts.is_empty() {
                    m.caption * 0.6
                } else {
                    m.caption * 2.0
                }
        }
        None => 0.0,
    };
    let composer_h = strip_h + card_h;
    // The transcript runs to the top of the screen; the floating header (tabs, avatar, status)
    // is drawn over it, so content starts below `top_inset`.
    let header_h = 0.0;
    let transcript = Rect::new(body.x, body.y, body.w, body.h - composer_h);

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
    let layout_key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (content_w.to_bits(), m.body.to_bits(), m.row.to_bits()).hash(&mut h);
        h.finish()
    };
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
        let (title, hint) = match link {
            LinkState::NotPaired => ("Not paired with Muse", "Pair in Settings, Profiles"),
            LinkState::Reconnecting => ("Reconnecting to Muse…", ""),
            _ => ("Can't reach Muse", "See Status"),
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

    if let (false, Some((secs, level))) = (keyboard_open, recorder) {
        // Recording a voice note: the composer is the recorder (no reply choices meanwhile).
        let strip = Rect::new(body.x, transcript.bottom(), body.w, composer_h);
        let (_, composer_row) = strip.inset(pad, pad * 0.5).split_top(m.row * 0.9);
        let bar = Rect::new(
            composer_row.x,
            composer_row.y + pad * 0.3,
            composer_row.w,
            m.row,
        );
        p.recorder_bar(bar, secs, level);
    } else if !keyboard_open {
        if let Some(item) = request {
            let card = Rect::new(
                body.x + pad,
                transcript.bottom(),
                card_w,
                card_h - pad * 0.3,
            );
            let (head, facts) = (&item.heading, &item.facts);
            let key = sprite_key(&(0xA5u8, head, facts, &item.body, layout_key));
            p.sprite(key, card.inflate(2.0), false, |p, r| {
                let card = r.inflate(-2.0);
                let pal = p.theme.pal;
                p.canvas.fill_rrect(card, m.radius_card * 0.8, pal.surface);
                p.canvas
                    .stroke_rrect(card, m.radius_card * 0.8, m.hairline * 2.0, pal.accent);
                let isz = m.body * 1.1;
                let x = card.x + pad * 0.8;
                let mut y = card.y + pad * 0.6;
                p.icon(Icon::Shield, isz, x, y, pal.accent);
                p.line(
                    head,
                    TextRole::BodyStrong,
                    x + isz + pad * 0.5,
                    y - m.body * 0.1,
                    card.w - isz - pad * 2.2,
                    pal.text,
                );
                y += m.body * 1.5;
                y += p.paragraph(
                    &item.body,
                    TextRole::Caption,
                    x + isz + pad * 0.5,
                    y,
                    card_inner - isz,
                    pal.text,
                );
                if facts.is_empty() {
                    return 0.0;
                }
                p.line(
                    facts,
                    TextRole::Caption,
                    x + isz + pad * 0.5,
                    y + m.caption * 0.5,
                    card.w - isz - pad * 2.2,
                    pal.text_muted,
                );
                0.0
            });
        }
        let strip = Rect::new(body.x, transcript.bottom() + card_h, body.w, strip_h);
        // The reply choices and the composer: one piece, redrawn when what it shows or
        // what is focused in it changes. Its focus targets are registered every frame.
        let strip_key = sprite_key(&(
            0xC0u8,
            composer,
            quick,
            request.is_some(),
            p.ui.focus.current(),
            data.call().is_some(),
            layout_key,
        ));
        p.sprite(strip_key, strip, true, |p, strip| {
            // Quick-reply chips row.
            let (chips_row, composer_row) = strip.inset(pad, pad * 0.5).split_top(m.row * 0.9);
            let mut cx = chips_row.x;
            let chip_h = m.row * 0.72;
            for (i, q) in quick.iter().enumerate() {
                // The answers to a request carry the button that gives them.
                let key = request
                    .and_then(|r| r.answers.get(i))
                    .and_then(|a| answer_button(&a.id));
                let kh = chip_h * 0.74;
                let kw = key.map_or(0.0, |b| p.keycap_width(b, kh) + pad * 0.4);
                let label_w = p.measure(q, TextRole::Body);
                let w = label_w + kw + pad * 1.6;
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

/// Generic vertical list of cards with a title and body.
fn card_list(
    p: &mut Painter,
    body: Rect,
    scroll: f32,
    _title: &str,
    rows: &[(String, String, Option<Icon>)],
) -> f32 {
    let m = p.m();
    let pad = m.pad;
    let clip = begin(p, body);
    let mut y = body.y + pad - scroll;
    let w = body.w - pad * 2.0;
    for (i, (t, d, icon)) in rows.iter().enumerate() {
        let inner = w - pad * 2.0 - icon.map(|_| m.body * 1.6).unwrap_or(0.0);
        let bodyh = p.paragraph_measure(d, TextRole::Caption, inner);
        let card_h = m.body * 1.5 + bodyh + pad * 1.4;
        let card = Rect::new(body.x + pad, y, w, card_h);
        let id = i as u64;
        let focused = p.ui.focus.is_focused(id);
        p.card(card);
        if focused {
            p.focus_ring(card, m.radius_card);
        }
        let mut tx = card.x + pad * 1.2;
        if let Some(ic) = icon {
            let isz = m.body * 1.5;
            p.icon(*ic, isz, tx, card.y + pad, p.theme.pal.accent);
            tx += isz + pad * 0.6;
        }
        let tw = card.right() - tx - pad;
        p.line(
            t,
            TextRole::BodyStrong,
            tx,
            card.y + pad * 0.9,
            tw,
            p.theme.pal.text,
        );
        p.paragraph(
            d,
            TextRole::Caption,
            tx,
            card.y + pad * 0.9 + m.body * 1.4,
            tw,
            p.theme.pal.text_muted,
        );
        p.focusable(id, card, 1);
        y += card_h + pad * 0.8;
    }
    end(p, clip);
    (y + scroll - body.y).max(body.h)
}

/// Feed (account mode).
pub fn feed(p: &mut Painter, data: &dyn DataProvider, body: Rect, scroll: f32) -> f32 {
    if data.account_stale() {
        return stale(p, body, "Feed");
    }
    let rows: Vec<(String, String, Option<Icon>)> = data
        .feed()
        .iter()
        .map(|f| {
            (
                f.title.clone(),
                format!("{}\n{} ago", f.body, f.age),
                Some(Icon::Feed),
            )
        })
        .collect();
    card_list(p, body, scroll, "Feed", &rows)
}

/// Ideas (account mode).
pub fn ideas(p: &mut Painter, data: &dyn DataProvider, body: Rect, scroll: f32) -> f32 {
    if data.account_stale() {
        return stale(p, body, "Ideas");
    }
    let rows: Vec<(String, String, Option<Icon>)> = data
        .ideas()
        .iter()
        .map(|i| (i.title.clone(), i.detail.clone(), Some(Icon::Ideas)))
        .collect();
    card_list(p, body, scroll, "Ideas", &rows)
}

/// Goals (account mode).
pub fn goals(p: &mut Painter, data: &dyn DataProvider, body: Rect, scroll: f32) -> f32 {
    if data.account_stale() {
        return stale(p, body, "Goals");
    }
    let m = p.m();
    let pad = m.pad;
    let clip = begin(p, body);
    let mut y = body.y + pad - scroll;
    let w = body.w - pad * 2.0;
    for (i, g) in data.goals().iter().enumerate() {
        let inner = w - pad * 2.0 - m.body * 1.6;
        let dh = p.paragraph_measure(&g.detail, TextRole::Caption, inner);
        let card_h = m.body * 1.5 + dh + pad * 1.4;
        let card = Rect::new(body.x + pad, y, w, card_h);
        let id = i as u64;
        let focused = p.ui.focus.is_focused(id);
        p.card(card);
        if focused {
            p.focus_ring(card, m.radius_card);
        }
        let bx = card.x + pad * 1.2;
        let bs = m.body * 1.1;
        if g.done {
            p.icon(Icon::CircleCheck, bs, bx, card.y + pad, p.theme.pal.ok);
        } else {
            p.canvas.stroke_circle(
                bx + bs * 0.5,
                card.y + pad + bs * 0.5,
                bs * 0.5,
                m.hairline.max(1.5),
                p.theme.pal.divider,
            );
        }
        let tx = bx + bs + pad * 0.7;
        let tw = card.right() - tx - pad;
        p.line(
            &g.title,
            TextRole::BodyStrong,
            tx,
            card.y + pad * 0.9,
            tw,
            p.theme.pal.text,
        );
        p.paragraph(
            &g.detail,
            TextRole::Caption,
            tx,
            card.y + pad * 0.9 + m.body * 1.4,
            tw,
            p.theme.pal.text_muted,
        );
        p.focusable(id, card, 1);
        y += card_h + pad * 0.8;
    }
    end(p, clip);
    (y + scroll - body.y).max(body.h)
}

/// Library (account mode).
pub fn library(p: &mut Painter, data: &dyn DataProvider, body: Rect, scroll: f32) -> f32 {
    if data.account_stale() {
        return stale(p, body, "Library");
    }
    let m = p.m();
    let pad = m.pad;
    let clip = begin(p, body);
    let mut y = body.y + pad - scroll;
    let w = body.w - pad * 2.0;
    let rowh = m.row * 1.2;
    for (i, it) in data.library().iter().enumerate() {
        let r = Rect::new(body.x + pad, y, w, rowh);
        let id = i as u64;
        let focused = p.ui.focus.is_focused(id);
        let icon = match it.kind.as_str() {
            "CSV" => Icon::File,
            "Image" => Icon::Image,
            "Space" => Icon::Library,
            _ => Icon::File,
        };
        let row = ListRow {
            icon: Some(icon),
            icon_color: Some(p.theme.pal.accent),
            title: &it.name,
            subtitle: &it.kind,
            chevron: true,
        };
        p.list_row(r, &row, focused);
        p.focusable(id, r, 1);
        y += rowh + m.hairline * 2.0;
    }
    end(p, clip);
    (y + scroll - body.y).max(body.h)
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
        && ((year == now.0 && yday + 1 == now.1) || (year + 1 == now.0 && now.1 == 0))
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

/// Focus id of the "New side chat" row (side chat rows use their index).
pub const NEW_SIDE_CHAT: u64 = 900;

/// Side chats list.
pub fn side_chats(p: &mut Painter, data: &dyn DataProvider, body: Rect, scroll: f32) -> f32 {
    let m = p.m();
    let pad = m.pad;
    let clip = begin(p, body);
    let mut y = body.y + pad - scroll;
    let w = body.w - pad * 2.0;
    let rowh = m.row * 1.2;
    {
        let r = Rect::new(body.x + pad, y, w, rowh);
        let focused = p.ui.focus.is_focused(NEW_SIDE_CHAT);
        let row = ListRow {
            icon: Some(Icon::Plus),
            icon_color: Some(p.theme.pal.accent),
            title: "New side chat",
            subtitle: "",
            chevron: false,
        };
        p.list_row(r, &row, focused);
        p.focusable(NEW_SIDE_CHAT, r, 1);
        y += rowh + m.hairline * 2.0;
    }
    for (i, c) in data.side_chats().iter().enumerate() {
        let r = Rect::new(body.x + pad, y, w, rowh);
        let id = i as u64;
        let focused = p.ui.focus.is_focused(id);
        let row = ListRow {
            icon: Some(Icon::Chat),
            icon_color: None,
            title: &c.title,
            subtitle: &c.when,
            chevron: true,
        };
        p.list_row(r, &row, focused);
        p.focusable(id, r, 1);
        y += rowh + m.hairline * 2.0;
    }
    end(p, clip);
    (y + scroll - body.y).max(body.h)
}

/// Notifications inbox.
pub fn notifications(p: &mut Painter, data: &dyn DataProvider, body: Rect, scroll: f32) -> f32 {
    let m = p.m();
    let pad = m.pad;
    if data.notifications().is_empty() {
        p.status_state(
            body,
            Icon::Inbox,
            "All caught up",
            "",
            p.theme.pal.text_muted,
        );
        return body.h;
    }
    let clip = begin(p, body);
    let mut y = body.y + pad - scroll;
    let w = body.w - pad * 2.0;
    let rowh = m.row * 1.5;
    for (i, n) in data.notifications().iter().enumerate() {
        let r = Rect::new(body.x + pad, y, w, rowh);
        let id = i as u64;
        let focused = p.ui.focus.is_focused(id);
        if focused {
            p.canvas
                .fill_rrect(r, m.radius_card * 0.7, p.theme.pal.subtle);
        }
        if !n.seen {
            p.canvas.fill_circle(
                r.x + pad * 0.6,
                r.cy(),
                m.caption * 0.35,
                p.theme.pal.accent,
            );
        }
        let tx = r.x + pad * 1.4;
        let tw = r.right() - tx - pad - p.measure(&n.age, TextRole::Caption);
        p.line(
            &n.title,
            TextRole::BodyStrong,
            tx,
            r.y + r.h * 0.22,
            tw,
            p.theme.pal.text,
        );
        p.line(
            &n.body,
            TextRole::Caption,
            tx,
            r.y + r.h * 0.56,
            r.right() - tx - pad,
            p.theme.pal.text_muted,
        );
        let aw = p.measure(&n.age, TextRole::Caption);
        p.line(
            &n.age,
            TextRole::Caption,
            r.right() - pad - aw,
            r.y + r.h * 0.22,
            aw + 2.0,
            p.theme.pal.text_muted,
        );
        if focused {
            p.focus_ring(r, m.radius_card * 0.7);
        }
        p.focusable(id, r, 1);
        y += rowh + m.hairline * 2.0;
    }
    end(p, clip);
    (y + scroll - body.y).max(body.h)
}

/// Device status screen.
pub fn status(p: &mut Painter, data: &dyn DataProvider, body: Rect, scroll: f32) -> f32 {
    let m = p.m();
    let pad = m.pad;
    let s = data.status();
    let clip = begin(p, body);
    let mut y = body.y + pad - scroll;
    let w = body.w - pad * 2.0;

    let link_txt = match s.link {
        LinkState::Connected => "Connected",
        LinkState::Reconnecting => "Reconnecting…",
        LinkState::NotPaired => "Not paired",
        LinkState::Error => "Error",
    };
    let battery_txt = if s.charging {
        format!("{}% · charging", s.battery)
    } else {
        format!("{}%", s.battery)
    };
    let fields: [(Icon, &str, &str); 6] = [
        (Icon::Wifi, "Link", link_txt),
        (Icon::Cpu, "VM", &s.vm),
        (Icon::Clock, "Token", &s.token_age),
        (Icon::Battery, "Battery", &battery_txt),
        (Icon::Storage, "Storage", &s.storage),
        (Icon::Thermometer, "Temperature", &s.temperature),
    ];
    let card = Rect::new(body.x + pad, y, w, m.row * fields.len() as f32 + pad);
    p.card(card);
    for (i, (icon, label, value)) in fields.iter().enumerate() {
        let r = Rect::new(
            card.x + pad,
            card.y + pad * 0.5 + i as f32 * m.row,
            card.w - pad * 2.0,
            m.row,
        );
        let isz = m.body * 1.1;
        p.icon(*icon, isz, r.x, r.cy() - isz * 0.5, p.theme.pal.text_muted);
        p.line(
            label,
            TextRole::Body,
            r.x + isz + pad * 0.6,
            r.cy() - m.body * 0.65,
            r.w * 0.4,
            p.theme.pal.text,
        );
        let vw = p.measure(value, TextRole::Body);
        p.line(
            value,
            TextRole::Body,
            r.right() - vw,
            r.cy() - m.body * 0.65,
            vw + 2.0,
            p.theme.pal.text_muted,
        );
        if i + 1 < fields.len() {
            p.divider(r.x, r.right(), r.bottom());
        }
    }
    y += card.h + pad;

    end(p, clip);
    (y + scroll - body.y).max(body.h)
}

/// Settings root.
/// What a profile's row says about it.
pub fn profile_state(p: &crate::data::ProfileEntry) -> &'static str {
    match (p.active, p.paired) {
        (true, true) => "In use",
        (true, false) => "In use · not paired",
        (false, true) => "Paired",
        (false, false) => "Not paired",
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
    /// How the app looks.
    Display,
    /// What draws attention: lights, vibration, speech.
    Alerts,
    /// The accounts on this handheld, and when it connects.
    Profiles,
    /// What Muse may do here as the profile in use.
    Access,
    /// The handheld and its connection, and the button test.
    Device,
}

impl SettingsTab {
    /// The pages in order.
    pub const ALL: [SettingsTab; 5] = [
        SettingsTab::Display,
        SettingsTab::Alerts,
        SettingsTab::Profiles,
        SettingsTab::Access,
        SettingsTab::Device,
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
        if row == id::INPUT_TEST {
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
            (
                id::CHAT_THEME,
                SettingRow::cycle("Chat colour", snap.chat_theme),
            ),
            (
                id::REDUCE_MOTION,
                SettingRow::toggle("Reduce motion", snap.reduce_motion),
            ),
            (0, SettingRow::section("AVATAR")),
            (id::AVATAR, SettingRow::cycle("Avatar size", snap.avatar)),
        ],
        SettingsTab::Alerts => vec![
            (0, SettingRow::section("WHEN CLOSED")),
            (id::LIGHTS, SettingRow::toggle("Blink lights", snap.lights)),
            (id::VIBRATE, SettingRow::toggle("Vibrate", snap.vibrate)),
            (0, SettingRow::section("SPEECH")),
            (
                id::VOICE_TO_VOICE,
                SettingRow::toggle("Speak replies to voice notes", snap.voice_to_voice),
            ),
            (
                id::CALL_GREETING,
                SettingRow::toggle("Greet on calls", snap.call_greeting),
            ),
        ],
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
        SettingsTab::Device => Vec::new(),
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
                id::CONSENT[3],
                SettingRow::toggle("Your commands", snap.consent.drop_ins)
                    .with_detail("Commands you wrote and put on this handheld"),
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
        rows.push((
            id::INPUT_TEST,
            SettingRow::value("Test buttons\u{2026}", ""),
        ));
    }
    if tab == SettingsTab::Access && !snap.builds.is_empty() {
        rows.push((0, SettingRow::section("BUILT BY MUSE")));
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

/// Appearance settings — these re-theme the whole UI live.
pub fn appearance(p: &mut Painter, theme: &Theme, body: Rect, scroll: f32) -> f32 {
    let m = p.m();
    let pad = m.pad;
    let clip = begin(p, body);
    let mut y = body.y + pad - scroll;
    let w = body.w - pad * 2.0;

    // Mode.
    p.line(
        "Mode",
        TextRole::Caption,
        body.x + pad,
        y,
        w,
        p.theme.pal.text_muted,
    );
    y += m.caption * 1.8;
    for (i, (label, mode)) in [("Light", Mode::Light), ("Dark", Mode::Dark)]
        .iter()
        .enumerate()
    {
        let r = Rect::new(body.x + pad, y, w, m.row);
        let id = 20 + i as u64;
        let focused = p.ui.focus.is_focused(id);
        let row = SettingRow::choice(label, theme.mode == *mode);
        p.setting_row(r, &row, focused);
        p.focusable(id, r, 1);
        y += m.row;
    }
    y += pad;

    // Chat theme swatches.
    p.line(
        "Chat theme",
        TextRole::Caption,
        body.x + pad,
        y,
        w,
        p.theme.pal.text_muted,
    );
    y += m.caption * 1.8;
    let sw = m.row * 1.1;
    let gap = pad * 0.7;
    let per = ((w + gap) / (sw + gap)).floor().max(1.0) as usize;
    for (i, ct) in goblin_ui_kit::theme::CHAT_THEMES.iter().enumerate() {
        let col = i % per;
        let rowi = i / per;
        let cxp = body.x + pad + col as f32 * (sw + gap);
        let cyp = y + rowi as f32 * (sw + gap);
        let r = Rect::new(cxp, cyp, sw, sw);
        let id = 40 + i as u64;
        let focused = p.ui.focus.is_focused(id);
        let color = if theme.mode == Mode::Light {
            ct.light
        } else {
            ct.dark
        };
        p.canvas.fill_circle(r.cx(), r.cy(), sw * 0.5, color);
        p.canvas
            .stroke_circle(r.cx(), r.cy(), sw * 0.5, m.hairline, p.theme.pal.divider);
        if ct.id == theme.chat_theme.id {
            p.canvas.stroke_circle(
                r.cx(),
                r.cy(),
                sw * 0.5 - m.focus,
                m.focus,
                p.theme.pal.accent,
            );
            let tick = if color.contrast(Color::BLACK) > color.contrast(Color::WHITE) {
                Color::BLACK
            } else {
                Color::WHITE
            };
            p.icon_centered(Icon::Check, sw * 0.5, r, tick);
        }
        if focused {
            p.focus_ring(r, sw * 0.5);
        }
        p.focusable(id, r, 1);
    }
    let rows = goblin_ui_kit::theme::CHAT_THEMES.len().div_ceil(per);
    y += rows as f32 * (sw + gap) + pad;

    // Avatar size.
    p.line(
        "Avatar size",
        TextRole::Caption,
        body.x + pad,
        y,
        w,
        p.theme.pal.text_muted,
    );
    y += m.caption * 1.8;
    for (i, size) in AvatarSize::ALL.iter().enumerate() {
        let r = Rect::new(body.x + pad, y, w, m.row);
        let id = 30 + i as u64;
        let focused = p.ui.focus.is_focused(id);
        let row = SettingRow::choice(size.label(), theme.avatar == *size);
        p.setting_row(r, &row, focused);
        p.focusable(id, r, 1);
        y += m.row;
    }
    end(p, clip);
    (y + scroll - body.y).max(body.h)
}

/// A stale-content placeholder for an M3 tab when the account credential is lost.
fn stale(p: &mut Painter, body: Rect, _name: &str) -> f32 {
    p.status_state(
        body,
        Icon::CloudOff,
        "Showing cached content",
        "Re-link your account",
        p.theme.pal.warn,
    );
    body.h
}

/// The input tester: every button lights up when held.
pub fn input_test(p: &mut Painter, held: &[Button], last: Option<Button>) {
    let m = p.m();
    let bounds = Rect::new(0.0, 0.0, p.canvas.width() as f32, p.canvas.height() as f32);
    p.line_centered(
        "Input test · Menu to exit",
        TextRole::Title,
        Rect::new(0.0, m.grid, bounds.w, m.grid * 4.0),
        p.theme.pal.text,
    );

    let cols = 6;
    let pad = m.grid * 2.0;
    let gw = (bounds.w - pad * 2.0) / cols as f32;
    let gh = m.row * 1.8;
    let start_y = m.grid * 7.0;
    for (i, b) in Button::ALL.iter().enumerate() {
        let col = i % cols;
        let row = i / cols;
        let cell = Rect::new(
            pad + col as f32 * gw,
            start_y + row as f32 * (gh + m.grid),
            gw - m.grid,
            gh,
        );
        let on = held.contains(b);
        let (bg, fg) = if on {
            (p.theme.pal.accent, p.theme.pal.on_accent)
        } else {
            (p.theme.pal.subtle, p.theme.pal.text)
        };
        p.canvas.fill_rrect(cell, m.radius_card, bg);
        if Some(*b) == last && !on {
            p.canvas
                .stroke_rrect(cell, m.radius_card, m.focus, p.theme.pal.accent.fade(0.5));
        }
        p.line_centered(b.name(), TextRole::BodyStrong, cell, fg);
    }
    let caption = format!("{} held", held.len());
    p.line_centered(
        &caption,
        TextRole::Caption,
        Rect::new(0.0, bounds.bottom() - m.grid * 4.0, bounds.w, m.grid * 3.0),
        p.theme.pal.text_muted,
    );
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
            time_label_from((126, 364, 11, 31, 23, 30), (127, 0), 2 * hour),
            "Yesterday 11:30 PM"
        );
        assert_eq!(
            time_label_from((126, 300, 9, 28, 12, 0), (126, 315), 400 * hour),
            "Oct 28, 12:00 PM"
        );
    }
}
