use std::collections::HashMap;

use bytes::Bytes;
use futures_util::stream::{Stream, StreamExt};
use indexmap::IndexMap;
use serde::Deserialize;
use snafu::ResultExt;

use crate::client::Docker;
use crate::error::{ApiSnafu, Error, JsonSnafu, Result};

/// One event from the Docker `/events` stream.
///
/// All fields are optional because the daemon emits a wide variety of event
/// shapes and we keep this type permissive.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct EventMessage {
    /// Object kind the event applies to — `container`, `image`, `volume`, ….
    #[serde(rename = "Type")]
    pub kind: Option<String>,
    /// What happened — `start`, `die`, `kill`, `destroy`, `health_status`, ….
    pub action: Option<String>,
    pub actor: EventActor,
    /// Unix seconds.
    #[serde(rename = "time")]
    pub time: Option<i64>,
    #[serde(rename = "timeNano")]
    pub time_nano: Option<i64>,
}

impl EventMessage {
    /// This event's time, formatted for [`EventsBuilder::since`]: unix
    /// seconds, with nanosecond precision when the daemon reported it. `None`
    /// if the event carries no timestamp at all.
    #[must_use]
    pub fn timestamp(&self) -> Option<String> {
        if let Some(nanos) = self.time_nano {
            let secs = nanos.div_euclid(1_000_000_000);
            let frac = nanos.rem_euclid(1_000_000_000);
            return Some(format!("{secs}.{frac:09}"));
        }
        self.time.map(|secs| secs.to_string())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct EventActor {
    #[serde(rename = "ID")]
    pub id: String,
    /// Includes labels copied onto the event by the daemon (e.g. `com.docker.compose.service`).
    #[serde(default)]
    pub attributes: IndexMap<String, String>,
}

/// Builder for [`Docker::events`].
pub struct EventsBuilder<'a> {
    docker: &'a Docker,
    filters: HashMap<&'static str, Vec<String>>,
    since: Option<String>,
}

impl Docker {
    /// `GET /events` — subscribe to the daemon's event stream.
    ///
    /// Returns a `Stream` of [`EventMessage`] values. Filters narrow the stream
    /// before the daemon sends bytes.
    #[must_use]
    pub fn events(&self) -> EventsBuilder<'_> {
        EventsBuilder {
            docker: self,
            filters: HashMap::new(),
            since: None,
        }
    }
}

impl EventsBuilder<'_> {
    #[must_use]
    pub fn with_label(mut self, key: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        self.filters.entry("label").or_default().push(format!(
            "{}={}",
            key.as_ref(),
            value.as_ref()
        ));
        self
    }

    #[must_use]
    pub fn with_label_key(mut self, key: impl Into<String>) -> Self {
        self.filters.entry("label").or_default().push(key.into());
        self
    }

    /// Filter on the actor object type (`container`, `image`, `volume`, ...).
    #[must_use]
    pub fn with_type(mut self, kind: impl Into<String>) -> Self {
        self.filters.entry("type").or_default().push(kind.into());
        self
    }

    /// Filter on the event action (`start`, `die`, ...).
    #[must_use]
    pub fn with_event(mut self, action: impl Into<String>) -> Self {
        self.filters.entry("event").or_default().push(action.into());
        self
    }

    /// Replay events from `ts` onward before streaming live ones. `ts` is a
    /// unix timestamp, optionally with fractional seconds
    /// (`1755385200.123456789`) — see [`EventMessage::timestamp`]. The daemon
    /// may include the event at exactly `ts`, so consumers must tolerate
    /// duplicates.
    #[must_use]
    pub fn since(mut self, ts: impl Into<String>) -> Self {
        self.since = Some(ts.into());
        self
    }

    /// Open the stream. The returned `Stream` yields one item per daemon event
    /// until the daemon closes the connection. A line the daemon sends that we
    /// can't parse yields [`Error::Json`] and the stream continues; a
    /// transport error yields that error and then ends.
    pub async fn call(self) -> Result<impl Stream<Item = Result<EventMessage>> + 'static> {
        let mut url = self.docker.url(["events"])?;
        if !self.filters.is_empty() {
            let json = serde_json::to_string(&self.filters).expect("string-keyed map serializes");
            url.query_pairs_mut().append_pair("filters", &json);
        }
        if let Some(since) = &self.since {
            url.query_pairs_mut().append_pair("since", since);
        }
        let response = self.docker.http().get(url).send().await?;
        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            return ApiSnafu {
                status: status.as_u16(),
                message,
            }
            .fail();
        }
        let bytes = response.bytes_stream().map(|r| r.map_err(Error::from));
        Ok(ndjson_lines(bytes))
    }
}

/// Split an NDJSON byte stream into events.
///
/// A line that doesn't deserialize is yielded as an error but leaves the
/// stream open: the daemon emits a wide variety of event shapes, and one we
/// don't understand must not cost the caller every subsequent event. Only a
/// transport error or end-of-body ends the stream (`done` in the state).
fn ndjson_lines<S>(stream: S) -> impl Stream<Item = Result<EventMessage>>
where
    S: Stream<Item = Result<Bytes>> + Unpin + 'static,
{
    futures_util::stream::unfold(
        (stream, Vec::<u8>::new(), false),
        |(mut stream, mut buf, done)| async move {
            if done {
                return None;
            }
            loop {
                if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=pos).collect();
                    let trimmed = trim_eol(&line);
                    if trimmed.is_empty() {
                        continue;
                    }
                    return Some((parse_event(trimmed), (stream, buf, false)));
                }
                match stream.next().await {
                    Some(Ok(chunk)) => buf.extend_from_slice(&chunk),
                    Some(Err(e)) => return Some((Err(e), (stream, buf, true))),
                    None => {
                        let trimmed = trim_eol(&buf);
                        if trimmed.is_empty() {
                            return None;
                        }
                        let last = parse_event(trimmed);
                        return Some((last, (stream, Vec::new(), true)));
                    }
                }
            }
        },
    )
}

fn parse_event(line: &[u8]) -> Result<EventMessage> {
    serde_json::from_slice(line).context(JsonSnafu {
        body: String::from_utf8_lossy(line).into_owned(),
    })
}

fn trim_eol(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && matches!(line[end - 1], b'\n' | b'\r') {
        end -= 1;
    }
    &line[..end]
}

#[cfg(test)]
mod tests {
    use futures_util::stream;

    use super::*;

    async fn lines(chunks: &[&str]) -> Vec<Result<EventMessage>> {
        let owned: Vec<Result<Bytes>> = chunks
            .iter()
            .map(|c| Ok(Bytes::copy_from_slice(c.as_bytes())))
            .collect();
        ndjson_lines(Box::pin(stream::iter(owned)))
            .collect::<Vec<_>>()
            .await
    }

    fn event(action: &str) -> String {
        format!(r#"{{"Type":"container","Action":"{action}","Actor":{{"ID":"abc"}}}}"#)
    }

    #[tokio::test]
    async fn unparseable_line_does_not_end_the_stream() {
        let body = format!("{}\nnot json\n{}\n", event("start"), event("die"));
        let got = lines(&[&body]).await;
        assert_eq!(got.len(), 3, "got {got:#?}");
        assert_eq!(
            got[0].as_ref().expect("first").action.as_deref(),
            Some("start")
        );
        assert!(matches!(got[1], Err(Error::Json { .. })), "got {got:#?}");
        assert_eq!(
            got[2].as_ref().expect("third").action.as_deref(),
            Some("die")
        );
    }

    #[tokio::test]
    async fn events_split_across_chunks() {
        let body = event("start");
        let (head, tail) = body.split_at(10);
        let got = lines(&[head, tail, "\n"]).await;
        assert_eq!(got.len(), 1, "got {got:#?}");
        assert_eq!(
            got[0].as_ref().expect("event").action.as_deref(),
            Some("start")
        );
    }

    #[test]
    fn timestamp_uses_nanos_when_present() {
        let ev: EventMessage = serde_json::from_str(
            r#"{"Type":"container","Action":"die","Actor":{"ID":"abc"},"time":1755385200,"timeNano":1755385200123456789}"#,
        )
        .expect("deserialize");
        assert_eq!(ev.timestamp().as_deref(), Some("1755385200.123456789"));
    }

    #[test]
    fn timestamp_falls_back_to_seconds() {
        let ev: EventMessage = serde_json::from_str(
            r#"{"Type":"container","Action":"die","Actor":{"ID":"abc"},"time":1755385200}"#,
        )
        .expect("deserialize");
        assert_eq!(ev.timestamp().as_deref(), Some("1755385200"));
    }
}
