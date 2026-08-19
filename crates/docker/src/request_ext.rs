use reqwest::{RequestBuilder, StatusCode};
use serde::de::DeserializeOwned;
use snafu::ResultExt;

use crate::Error;
use crate::error::{ApiSnafu, JsonSnafu};

pub(crate) trait ReqwestExt {
    async fn try_send<T: DeserializeOwned>(self) -> crate::Result<T>;
    async fn try_send_empty(self) -> crate::Result<()>;
    /// As [`Self::try_send_empty`], but for state changes the daemon reports
    /// as 304 when the object is already in the requested state.
    async fn try_send_empty_or_unchanged(self) -> crate::Result<()>;
    /// Drain a newline-delimited JSON response, parsing each non-empty line
    /// into `T`. Drops any blank lines.
    async fn try_send_ndjson<T: DeserializeOwned>(self) -> crate::Result<Vec<T>>;
    /// Send and validate, handing back the response with its body unread, for
    /// endpoints whose output is worth reporting as it arrives.
    async fn try_send_streaming(self) -> crate::Result<reqwest::Response>;
}

impl ReqwestExt for RequestBuilder {
    async fn try_send<T: DeserializeOwned>(self) -> crate::Result<T> {
        let body = check_response_body(self).await?;
        serde_json::from_slice(&body).with_context(|_| JsonSnafu {
            body: String::from_utf8_lossy(&body).into_owned(),
        })
    }

    async fn try_send_empty(self) -> crate::Result<()> {
        check_response_body(self).await.map(drop)
    }

    async fn try_send_empty_or_unchanged(self) -> crate::Result<()> {
        let response = self.send().await?;
        if response.status() == StatusCode::NOT_MODIFIED {
            return Ok(());
        }
        check_status(response).await.map(drop)
    }

    async fn try_send_ndjson<T: DeserializeOwned>(self) -> crate::Result<Vec<T>> {
        let body = check_response_body(self).await?;
        body.split(|b| *b == b'\n')
            .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
            .map(|line| {
                serde_json::from_slice(line).with_context(|_| JsonSnafu {
                    body: String::from_utf8_lossy(line).into_owned(),
                })
            })
            .collect()
    }

    async fn try_send_streaming(self) -> crate::Result<reqwest::Response> {
        check_response(self).await
    }
}

/// Send and validate; returns the response with its body unread. Maps 404 to
/// [`Error::NotFound`] and other non-success statuses to [`Error::Api`].
async fn check_response(request: RequestBuilder) -> crate::Result<reqwest::Response> {
    check_status(request.send().await?).await
}

/// Classify a response that has already been sent.
async fn check_status(response: reqwest::Response) -> crate::Result<reqwest::Response> {
    let status = response.status();
    if status == StatusCode::NOT_FOUND {
        return Err(Error::NotFound);
    }
    if !status.is_success() {
        let message = response.text().await.unwrap_or_default();
        return ApiSnafu {
            status: status.as_u16(),
            message,
        }
        .fail();
    }
    Ok(response)
}

/// As [`check_response`], reading the body to completion.
async fn check_response_body(request: RequestBuilder) -> crate::Result<bytes::Bytes> {
    Ok(check_response(request).await?.bytes().await?)
}
