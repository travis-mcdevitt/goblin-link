//! Muse inline option widgets translated to plain chat text and reply choices.

use serde_json::Value;

/// A message after resolving its inline widget references. No widget code is executed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Choices {
    /// Display text with widget tokens replaced by readable text.
    pub text: String,
    /// Short choices that fit the handheld's reply row.
    pub replies: Vec<String>,
}

/// Resolve only widgets explicitly referenced by this message. Unknown or malformed
/// widgets remain visible as a phone handoff rather than disappearing from the reply.
pub fn decode(text: &str, payload: &Value) -> Choices {
    if text.len() > 64 * 1024 || text.matches("[[hatch_widget:").count() > 32 {
        return Choices {
            text: "[Widget — open Muse on your phone]".into(),
            replies: Vec::new(),
        };
    }
    let widgets = payload
        .get("widgets")
        .and_then(Value::as_array)
        .or_else(|| {
            payload
                .pointer("/payload/widgets")
                .and_then(Value::as_array)
        });
    let mut out = Choices::default();
    let mut rest = text;
    while let Some(start) = rest.find("[[hatch_widget:") {
        out.text.push_str(&rest[..start]);
        let token = &rest[start + "[[hatch_widget:".len()..];
        let Some(end) = token.find("]]") else {
            out.text.push_str("[Widget — open Muse on your phone]");
            rest = "";
            break;
        };
        let id = &token[..end];
        let widget = widgets.into_iter().flatten().take(32).find(|w| {
            w.get("widget_id")
                .or_else(|| w.get("id"))
                .and_then(Value::as_str)
                == Some(id)
        });
        let options = widget
            .filter(|w| w.get("kind").and_then(Value::as_str) == Some("option"))
            .and_then(|w| w.pointer("/data/options"))
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty() && a.len() <= 6)
            .and_then(|a| {
                a.iter()
                    .map(|v| {
                        v.as_str()
                            .filter(|s| {
                                !s.trim().is_empty()
                                    && s.chars().count() <= 240
                                    && !s.chars().any(char::is_control)
                            })
                            .map(str::to_owned)
                    })
                    .collect::<Option<Vec<_>>>()
            });
        if let Some(options) = options {
            // Keep all choices readable even when the compact button row cannot fit them.
            out.text.push_str(
                &options
                    .iter()
                    .map(|s| format!("• {s}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            out.replies.extend(options);
        } else {
            out.text.push_str("[Widget — open Muse on your phone]");
        }
        rest = &token[end + 2..];
    }
    out.text.push_str(rest);
    if out.text.len() > 64 * 1024 {
        return Choices {
            text: "[Widget — open Muse on your phone]".into(),
            replies: Vec::new(),
        };
    }
    if out.replies.len() > 6 || out.replies.iter().map(|s| s.chars().count()).sum::<usize>() > 60 {
        out.replies.clear();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_referenced_text_options_become_replies() {
        let payload = json!({"widgets": [
            {"widget_id":"w", "kind":"option", "data":{"options":["Put it on the Brick", "Make it a web page", "Surprise me"]}},
            {"widget_id":"other", "kind":"option", "data":{"options":["Do something else"]}}
        ]});
        let result = decode("Where?\n[[hatch_widget:w]]", &payload);
        assert_eq!(
            result.replies,
            ["Put it on the Brick", "Make it a web page", "Surprise me"]
        );
        assert!(!result.text.contains("hatch_widget"));
        assert!(result.text.contains("Surprise me"));
        assert!(decode("No choices", &payload).replies.is_empty());
        assert_eq!(
            decode(
                "[[hatch_widget:w]]",
                &json!({"widgets":null, "payload":payload})
            )
            .replies,
            result.replies
        );
        assert_eq!(
            decode("[[hatch_widget:w]]", &json!({"payload":payload})).replies,
            result.replies
        );
    }

    #[test]
    fn malformed_unsupported_and_long_widgets_have_no_actions() {
        for data in [
            json!({"kind":"html", "data":{"html":"<script>run()</script>"}}),
            json!({"kind":"option", "data":{"options":["Yes", 1]}}),
            json!({"kind":"option", "data":{"options":["line\nbreak"]}}),
        ] {
            let mut w = data;
            w["widget_id"] = json!("w");
            let result = decode("[[hatch_widget:w]]", &json!({"widgets":[w]}));
            assert!(result.replies.is_empty());
            assert!(result.text.contains("open Muse"));
        }
        let result = decode(
            "[[hatch_widget:w]]",
            &json!({"widgets":[{"widget_id":"w", "kind":"option", "data":{"options":["x".repeat(61)]}}]}),
        );
        assert!(result.replies.is_empty());
        assert!(result.text.contains(&"x".repeat(61)));
        assert!(decode("[[hatch_widget:unfinished", &Value::Null)
            .text
            .contains("open Muse"));
    }
}
