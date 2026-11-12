//! Line decoding and event rendering for the terminal client.

use mika_core::Event;

/// UTF-8 with replacement of broken bytes, without the trailing line break.
pub fn decode_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.trim_end_matches(['\r', '\n']).to_string()
}

/// Turns events into terminal text. Streamed pieces of a reply stay on one line.
#[derive(Default)]
pub struct Renderer {
    in_reply: bool,
}

impl Renderer {
    /// Text to print as is; it carries its own line breaks.
    pub fn render(&mut self, event: &Event) -> String {
        if let Event::AssistantDelta { text } = event {
            let prefix = if self.in_reply { "" } else { "[mika] " };
            self.in_reply = true;
            return format!("{prefix}{text}");
        }
        if let Event::AssistantText { text } = event {
            let out = if self.in_reply {
                "\n".to_string()
            } else {
                format!("[mika] {text}\n")
            };
            self.in_reply = false;
            return out;
        }
        let line = match event {
            Event::StateChanged { from, to } => {
                format!("[state] {} -> {}", from.as_str(), to.as_str())
            }
            Event::UserText { text } => format!("[user] {text}"),
            Event::Error { message } => format!("[error] {message}"),
            Event::ToolStarted { name, summary, .. } => format!("[tool] {name}: {summary}"),
            Event::ToolFinished {
                name,
                ok,
                elapsed_ms,
                output,
                ..
            } => {
                let status = if *ok { "ok" } else { "fail" };
                format!("[tool] {name} {status} {elapsed_ms}ms: {output}")
            }
            Event::ConfirmRequested {
                assessment: a,
                allow_always,
                ..
            } => {
                let options = if *allow_always {
                    "да / разреши всегда / нет"
                } else {
                    "да / нет"
                };
                format!(
                    "[confirm] {}: {} (риск {}: {}) — {options}",
                    a.summary,
                    a.target,
                    a.level.as_str(),
                    a.reason
                )
            }
            Event::ConfirmResolved { answer, by, .. } => {
                format!("[confirm] {answer:?} ({})", by.as_str()).to_lowercase()
            }
            Event::ReminderFired { text, late_s, .. } => {
                format!("[reminder] {text} (late {late_s} s)")
            }
            Event::UiTarget { x, y, w, h } => format!("[target] {x},{y} {w}x{h}"),
            Event::Timing { stage, ms } => format!("[time] {stage} {ms} ms"),
            Event::TaskUpdated { task } => {
                let done = task
                    .steps
                    .iter()
                    .filter(|s| s.status == mika_core::task::StepStatus::Done)
                    .count();
                let state = if task.finished {
                    "закончена"
                } else {
                    "идёт"
                };
                format!(
                    "[task] {} — {done}/{} — {state}",
                    task.title,
                    task.steps.len()
                )
            }
            Event::EmotionCue { emotion } | Event::EmotionChanged { emotion } => {
                format!("[emotion] {}", emotion.as_str())
            }
            Event::AssistantDelta { .. } | Event::AssistantText { .. } => unreachable!(),
        };
        let lead = if self.in_reply { "\n" } else { "" };
        self.in_reply = false;
        format!("{lead}{line}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mika_core::AssistantState;

    #[test]
    fn decode_line_survives_invalid_utf8() {
        assert_eq!(decode_line(b"ok\r\n"), "ok");
        assert_eq!(
            decode_line(&[0xFF, 0xFE, b'o', b'k', b'\n']),
            "\u{fffd}\u{fffd}ok"
        );
    }

    #[test]
    fn simple_events_match_table() {
        let mut r = Renderer::default();
        assert_eq!(
            r.render(&Event::StateChanged {
                from: AssistantState::Idle,
                to: AssistantState::AwaitingConfirm
            }),
            "[state] idle -> awaiting_confirm\n"
        );
        assert_eq!(
            r.render(&Event::UserText {
                text: "привет".into()
            }),
            "[user] привет\n"
        );
        assert_eq!(
            r.render(&Event::Error {
                message: "x".into()
            }),
            "[error] x\n"
        );
    }

    #[test]
    fn deltas_stay_on_one_line_and_final_text_ends_it() {
        let mut r = Renderer::default();
        let out: String = [
            Event::AssistantDelta {
                text: "При".into()
            },
            Event::AssistantDelta {
                text: "вет".into()
            },
            Event::AssistantText {
                text: "Привет".into(),
            },
        ]
        .iter()
        .map(|e| r.render(e))
        .collect();
        assert_eq!(out, "[mika] Привет\n");
    }

    #[test]
    fn final_text_without_deltas_is_printed_whole() {
        let mut r = Renderer::default();
        assert_eq!(
            r.render(&Event::AssistantText {
                text: "ок".into()
            }),
            "[mika] ок\n"
        );
    }

    #[test]
    fn confirm_request_lists_answers() {
        use mika_core::tools::{Assessment, RiskLevel};
        let mut r = Renderer::default();
        let line = r.render(&Event::ConfirmRequested {
            id: 1,
            tool: "power".into(),
            assessment: Assessment::new(
                RiskLevel::High,
                "Перезагрузить",
                "компьютер",
                "нельзя отменить",
            ),
            allow_always: false,
            timeout_s: 60,
        });
        assert_eq!(
            line,
            "[confirm] Перезагрузить: компьютер (риск high: нельзя отменить) — да / нет
"
        );
    }

    #[test]
    fn other_event_breaks_streamed_line() {
        let mut r = Renderer::default();
        r.render(&Event::AssistantDelta {
            text: "Смотрю".into(),
        });
        assert_eq!(
            r.render(&Event::ToolFinished {
                id: "1".into(),
                name: "get_time".into(),
                ok: true,
                output: "12:00".into(),
                elapsed_ms: 3,
                data: None,
            }),
            "\n[tool] get_time ok 3ms: 12:00\n"
        );
    }
}
