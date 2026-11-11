//! Server-sent events parser that accepts arbitrary byte chunks.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds bytes and returns every event completed by them.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() || self.event.is_some() {
                    out.push(SseEvent {
                        event: self.event.take(),
                        data: std::mem::take(&mut self.data).join("\n"),
                    });
                }
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                None => (line, ""),
            };
            match field {
                "event" => self.event = Some(value.to_string()),
                "data" => self.data.push(value.to_string()),
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_splits_events_across_chunks() {
        let mut p = SseParser::new();
        assert!(p.push(b"data: {\"a\"").is_empty());
        let ev = p.push(b":1}\r\n\r\ndata: x\n\n");
        assert_eq!(
            ev,
            vec![
                SseEvent {
                    event: None,
                    data: "{\"a\":1}".into()
                },
                SseEvent {
                    event: None,
                    data: "x".into()
                },
            ]
        );
    }

    #[test]
    fn sse_joins_multiline_data_and_keeps_event_name() {
        let mut p = SseParser::new();
        let ev = p.push(b"event: content_block_delta\ndata: a\ndata: b\n\n");
        assert_eq!(
            ev,
            vec![SseEvent {
                event: Some("content_block_delta".into()),
                data: "a\nb".into()
            }]
        );
    }

    #[test]
    fn sse_ignores_comments() {
        let mut p = SseParser::new();
        assert_eq!(p.push(b": ping\n\ndata: y\n\n").len(), 1);
    }
}
