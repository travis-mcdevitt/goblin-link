//! Typed request methods and events with their wire names.

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::types::*;

/// Protocol version carried in `hello`'s result.
pub const PROTOCOL_VERSION: u32 = 1;

/// A request parameter type tied to its wire method name and result type.
pub trait Request: Serialize + DeserializeOwned + Into<Method> {
    /// Wire name, e.g. `"chat.send"`.
    const NAME: &'static str;
    /// Type of the success result.
    type Result: Serialize + DeserializeOwned;
}

fn params_or_empty(params: Value) -> Value {
    if params.is_null() {
        Value::Object(Default::default())
    } else {
        params
    }
}

fn bad_request(name: &str, e: serde_json::Error) -> IpcError {
    IpcError::new(ErrorCode::BadRequest, format!("{name}: {e}"))
}

macro_rules! methods {
    ($($var:ident($name:literal, $p:ty => $r:ty)),+ $(,)?) => {
        /// Every request the daemon understands, with typed parameters.
        #[derive(Debug, Clone, PartialEq)]
        pub enum Method {
            $(
                #[doc = concat!("`", $name, "`")]
                $var($p),
            )+
        }

        $(
            impl Request for $p {
                const NAME: &'static str = $name;
                type Result = $r;
            }
            impl From<$p> for Method {
                fn from(p: $p) -> Self { Method::$var(p) }
            }
        )+

        impl Method {
            /// Wire names of all methods, in documentation order.
            pub const NAMES: &'static [&'static str] = &[$($name),+];

            /// The wire name of this request.
            pub fn name(&self) -> &'static str {
                match self { $(Method::$var(_) => $name,)+ }
            }

            /// The parameters as a JSON object.
            pub fn params_value(&self) -> Result<Value, serde_json::Error> {
                match self { $(Method::$var(p) => serde_json::to_value(p),)+ }
            }

            /// Parses a request. Unknown names give `unknown_method`; invalid params `bad_request`.
            /// Missing (`null`) params are treated as `{}`.
            pub fn from_parts(name: &str, params: Value) -> Result<Method, IpcError> {
                let params = params_or_empty(params);
                match name {
                    $($name => serde_json::from_value::<$p>(params)
                        .map(Method::$var)
                        .map_err(|e| bad_request(name, e)),)+
                    other => Err(IpcError::new(
                        ErrorCode::UnknownMethod,
                        format!("unknown method {other}"),
                    )),
                }
            }

            /// Checks that `result` has the documented shape for method `name` and round-trips
            /// through the typed result without losing fields.
            pub fn check_result(name: &str, result: &Value) -> Result<(), String> {
                match name {
                    $($name => roundtrip::<$r>(result),)+
                    other => Err(format!("unknown method {other}")),
                }
            }
        }
    };
}

pub(crate) fn roundtrip<T: Serialize + DeserializeOwned>(v: &Value) -> Result<(), String> {
    let typed: T = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
    let back = serde_json::to_value(&typed).map_err(|e| e.to_string())?;
    if &back == v {
        Ok(())
    } else {
        Err(format!("round trip changed value: {v} -> {back}"))
    }
}

methods! {
    Hello("hello", HelloParams => HelloResult),
    Subscribe("subscribe", SubscribeParams => SubscribeResult),
    DaemonStatus("daemon.status", DaemonStatusParams => Status),
    DaemonShutdown("daemon.shutdown", DaemonShutdownParams => Empty),

    ProfileList("profile.list", ProfileListParams => ProfileListResult),
    ProfileCreate("profile.create", ProfileCreateParams => Empty),
    ProfileRename("profile.rename", ProfileRenameParams => Empty),
    ProfileDelete("profile.delete", ProfileDeleteParams => Empty),
    ProfileSwitch("profile.switch", ProfileSwitchParams => Empty),
    ProfileSetSdkToken("profile.set_sdk_token", ProfileSetSdkTokenParams => Empty),
    PairStart("pair.start", PairStartParams => PairStartResult),
    PairCancel("pair.cancel", PairCancelParams => Empty),
    PairUnpair("pair.unpair", PairUnpairParams => Empty),

    ChatList("chat.list", ChatListParams => ChatListResult),
    ChatCreate("chat.create", ChatCreateParams => ChatCreateResult),
    ChatForget("chat.forget", ChatForgetParams => Empty),
    ChatRename("chat.rename", ChatRenameParams => Empty),
    ChatHistory("chat.history", ChatHistoryParams => ChatHistoryResult),
    ChatSend("chat.send", ChatSendParams => ChatSendResult),
    ChatSendImage("chat.send_image", ChatSendImageParams => TurnResult),
    ChatSendVoice("chat.send_voice", ChatSendVoiceParams => TurnResult),
    ChatMarkSeen("chat.mark_seen", ChatMarkSeenParams => Empty),
    ChatCancel("chat.cancel", ChatCancelParams => Empty),
    ChatSuggest("chat.suggest", ChatSuggestParams => ChatSuggestResult),

    VoicePttStart("voice.ptt_start", VoicePttStartParams => VoicePttStartResult),
    VoicePttStop("voice.ptt_stop", VoicePttStopParams => VoicePttStopResult),
    VoiceSpeak("voice.speak", VoiceSpeakParams => Empty),
    VoiceStopSpeaking("voice.stop_speaking", VoiceStopSpeakingParams => Empty),
    VoiceCallStart("voice.call_start", VoiceCallStartParams => VoiceCallStartResult),
    VoiceCallEnd("voice.call_end", VoiceCallEndParams => VoiceCallEndResult),
    VoiceCallMute("voice.call_mute", VoiceCallMuteParams => Empty),
    VoiceBargeIn("voice.barge_in", VoiceBargeInParams => Empty),
    VoiceDevices("voice.devices", VoiceDevicesParams => VoiceDevicesResult),

    NotifyList("notify.list", NotifyListParams => NotifyListResult),
    NotifySeen("notify.seen", NotifySeenParams => Empty),
    NotifyClear("notify.clear", NotifyClearParams => Empty),

    CommandsLog("commands.log", CommandsLogParams => CommandsLogResult),
    CommandsConsentGet("commands.consent.get", ConsentGetParams => Consent),
    CommandsConsentSet("commands.consent.set", ConsentUpdate => Consent),

    SettingsGet("settings.get", SettingsGetParams => SettingsValues),
    SettingsSet("settings.set", SettingsSetParams => Empty),
    PlatformInfo("platform.info", PlatformInfoParams => FreeForm),
    PlatformVolume("platform.volume", PlatformVolumeParams => PercentResult),
    PlatformBrightness("platform.brightness", PlatformBrightnessParams => PercentResult),
    PlatformLedsTest("platform.leds_test", PlatformLedsTestParams => Empty),
    PlatformRumbleTest("platform.rumble_test", PlatformRumbleTestParams => Empty),
    PlatformAutostart("platform.autostart", PlatformAutostartParams => PlatformAutostartResult),
    PlatformHealth("platform.health", PlatformHealthParams => FreeForm),

    WorkshopList("workshop.list", WorkshopListParams => WorkshopListResult),
    WorkshopRequest("workshop.request", WorkshopRequestParams => WorkshopRequestResult),
    WorkshopModify("workshop.modify", WorkshopModifyParams => TurnResult),
    WorkshopLaunch("workshop.launch", WorkshopLaunchParams => Empty),
    WorkshopRollback("workshop.rollback", WorkshopRollbackParams => Empty),
    WorkshopRemove("workshop.remove", WorkshopRemoveParams => Empty),
    WorkshopLog("workshop.log", WorkshopLogParams => WorkshopLogResult),
    WorkshopInstall("workshop.install", WorkshopInstallParams => Empty),

    AccountStatus("account.status", AccountStatusParams => AccountState),
    AccountLinkStart("account.link_start", AccountLinkStartParams => AccountLinkStartResult),
    AccountLinkCancel("account.link_cancel", AccountLinkCancelParams => Empty),
    AccountUnlink("account.unlink", AccountUnlinkParams => Empty),
    MirrorGet("mirror.get", MirrorGetParams => MirrorGetResult),
    MirrorRefresh("mirror.refresh", MirrorRefreshParams => Empty),
    MirrorAct("mirror.act", MirrorActParams => MirrorActResult),
}

macro_rules! events {
    ($($var:ident($name:literal, $topic:ident, $p:ty)),+ $(,)?) => {
        /// Every event the daemon emits, with typed payloads. Events with unknown names are
        /// preserved as [`Event::Unknown`] so that clients can ignore them.
        #[derive(Debug, Clone, PartialEq)]
        pub enum Event {
            $(
                #[doc = concat!("`", $name, "`")]
                $var($p),
            )+
            /// An event this version does not know.
            Unknown {
                /// Wire event name.
                event: String,
                /// Raw payload.
                data: Value,
            },
        }

        impl Event {
            /// Wire names of all known events.
            pub const NAMES: &'static [&'static str] = &[$($name),+];

            /// The wire name of this event.
            pub fn name(&self) -> &str {
                match self {
                    $(Event::$var(_) => $name,)+
                    Event::Unknown { event, .. } => event,
                }
            }

            /// The topic subscribers filter on ([`Topic::Unknown`] for unknown events).
            pub fn topic(&self) -> Topic {
                match self {
                    $(Event::$var(_) => Topic::$topic,)+
                    Event::Unknown { .. } => Topic::Unknown,
                }
            }

            /// The payload as JSON.
            pub fn data_value(&self) -> Result<Value, serde_json::Error> {
                match self {
                    $(Event::$var(p) => serde_json::to_value(p),)+
                    Event::Unknown { data, .. } => Ok(data.clone()),
                }
            }

            /// Parses an event; unknown names yield [`Event::Unknown`], a malformed payload of a
            /// known event is an error.
            pub fn from_parts(name: &str, data: Value) -> Result<Event, serde_json::Error> {
                match name {
                    $($name => serde_json::from_value::<$p>(data).map(Event::$var),)+
                    other => Ok(Event::Unknown { event: other.to_owned(), data }),
                }
            }

            /// Checks that `data` has the documented shape for event `name` and round-trips.
            pub fn check_data(name: &str, data: &Value) -> Result<(), String> {
                match name {
                    $($name => roundtrip::<$p>(data),)+
                    other => Err(format!("unknown event {other}")),
                }
            }
        }

    };
}

events! {
    ProfileChanged("daemon.profile_changed", Daemon, ProfileChangedEvent),
    LinkState("link.state", Link, LinkStateEvent),
    AgentActivity("agent.activity", Link, Activity),
    PairState("pair.state", Daemon, PairStateEvent),
    ChatMessage("chat.message", Chat, ChatMessageEvent),
    ChatDelta("chat.delta", Chat, ChatDeltaEvent),
    ChatTurn("chat.turn", Chat, ChatTurnEvent),
    ChatUnread("chat.unread", Chat, ChatUnreadEvent),
    ChatListChanged("chat.list_changed", Chat, Empty),
    ChatSuggestions("chat.suggestions", Chat, ChatSuggestionsEvent),
    VoiceLevel("voice.level", Voice, VoiceLevelEvent),
    VoiceNote("voice.note", Voice, VoiceNoteEvent),
    VoiceSpeaking("voice.speaking", Voice, VoiceSpeakingEvent),
    VoiceCall("voice.call", Voice, VoiceCallEvent),
    VoiceCaption("voice.caption", Voice, VoiceCaptionEvent),
    NotifyNew("notify.new", Notify, NotifyNewEvent),
    NotifyCount("notify.count", Notify, NotifyCountEvent),
    CommandsStarted("commands.started", Commands, CommandEvent),
    CommandsFinished("commands.finished", Commands, CommandEvent),
    SettingsChanged("settings.changed", Settings, SettingsValues),
    WorkshopChanged("workshop.changed", Workshop, WorkshopChangedEvent),
    WorkshopExited("workshop.exited", Workshop, WorkshopExitedEvent),
    AccountState("account.state", Account, AccountState),
    MirrorChanged("mirror.changed", Mirror, MirrorChangedEvent),
}
