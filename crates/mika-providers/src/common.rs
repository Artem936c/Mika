//! Shared HTTP plumbing for adapters.

use std::time::Duration;

use futures::StreamExt;
use mika_core::llm::{LlmChunk, LlmError, LlmStream};
use mika_core::secrets::Secret;
use tokio::sync::mpsc;

use crate::factory::redact;
use crate::sse::{SseEvent, SseParser};
use crate::trace::RequestLog;

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(90))
        .build()
        .expect("reqwest client")
}

pub fn network_error(e: reqwest::Error, key: Option<&Secret>) -> LlmError {
    // The URL can be part of the message; keys travel in headers, but redact anyway.
    LlmError::Network(redact(&e.without_url().to_string(), key))
}

/// Sends the request and turns a non-2xx answer into `LlmError::Http` with a redacted body.
pub async fn send(
    req: reqwest::RequestBuilder,
    key: Option<&Secret>,
) -> Result<reqwest::Response, LlmError> {
    let resp = req.send().await.map_err(|e| network_error(e, key))?;
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    Err(LlmError::Http {
        status: status.as_u16(),
        body: redact(&body, key),
    })
}

/// What a protocol handler says after each SSE event.
pub enum Step {
    Continue,
    Finished,
}

/// Reads the SSE body in a task and exposes the chunks as a stream.
/// Dropping the stream stops the task at once, also while the provider is
/// silent or sends only events that give no chunk (reasoning, pings): the
/// connection closes and the provider stops generating. A body that ends
/// before `Step::Finished` is reported as a network error, because the answer
/// was cut. `log` is the record of the request: it is written once, when the
/// task ends, and says how the answer ended.
pub(crate) fn sse_stream<H>(
    resp: reqwest::Response,
    key: Option<Secret>,
    mut log: RequestLog,
    mut handler: H,
) -> LlmStream
where
    H: FnMut(SseEvent, &mut Vec<LlmChunk>, &mut RequestLog) -> Result<Step, LlmError>
        + Send
        + 'static,
{
    let (tx, rx) = mpsc::channel::<Result<LlmChunk, LlmError>>(64);
    tokio::spawn(async move {
        let mut body = resp.bytes_stream();
        let mut parser = SseParser::new();
        let mut out = Vec::new();
        loop {
            let next = tokio::select! {
                // Nobody reads the answer any more.
                _ = tx.closed() => return,
                next = body.next() => next,
            };
            let bytes = match next {
                Some(Ok(b)) => b,
                Some(Err(e)) => {
                    let error = network_error(e, key.as_ref());
                    log.failed(&error);
                    let _ = tx.send(Err(error)).await;
                    return;
                }
                None => {
                    let error = LlmError::Network("соединение оборвалось посреди ответа".into());
                    log.failed(&error);
                    let _ = tx.send(Err(error)).await;
                    return;
                }
            };
            for event in parser.push(&bytes) {
                let step = handler(event, &mut out, &mut log);
                for chunk in out.drain(..) {
                    if tx.send(Ok(chunk)).await.is_err() {
                        return;
                    }
                }
                match step {
                    Ok(Step::Continue) => {}
                    Ok(Step::Finished) => {
                        log.finished();
                        return;
                    }
                    Err(e) => {
                        log.failed(&e);
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                }
            }
        }
    });
    futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|x| (x, rx)) }).boxed()
}
