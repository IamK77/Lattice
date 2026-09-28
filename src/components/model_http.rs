//! Shared pre-response retry discipline for the three model transports.
//! A successful HTTP response leaves this helper before its body is read;
//! stream failures are never replayed here. Intermediate failures stay inside
//! one model call, and cancellation wins over another attempt or a ready timer.
use std::fmt;
use std::future::Future;
use std::time::Duration;

use reqwest::{Client, Request, RequestBuilder, Response, StatusCode};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::model_common::{error_info, full_cause};

const MAX_RETRIES: usize = 10;
const INITIAL_DELAY: Duration = Duration::from_millis(200);
const MAX_DELAY: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub(super) enum SendError {
    Cancelled,
    Transport(reqwest::Error),
    NotReplayable,
}

impl SendError {
    pub(super) fn retryable(&self) -> bool {
        match self {
            Self::Transport(error) => !error.is_builder() && !error.is_redirect(),
            Self::Cancelled | Self::NotReplayable => false,
        }
    }
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("model request cancelled"),
            Self::Transport(error) => fmt::Display::fmt(error, f),
            Self::NotReplayable => f.write_str("model request body cannot be replayed"),
        }
    }
}

impl std::error::Error for SendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            _ => None,
        }
    }
}

fn retryable_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

/// Request bytes and headers are cloned, not rebuilt from changing input.
/// No response body is inspected until the retry loop has returned.
pub(super) async fn send(
    client: &Client,
    request: Request,
    token: &CancellationToken,
) -> Result<Response, SendError> {
    retry(
        token,
        || async {
            let copy = request.try_clone().ok_or(SendError::NotReplayable)?;
            client.execute(copy).await.map_err(SendError::Transport)
        },
        |outcome| match outcome {
            Ok(response) => retryable_status(response.status()),
            Err(error) => error.retryable(),
        },
    )
    .await
    .unwrap_or(Err(SendError::Cancelled))
}

async fn retry<T, E, F, Fut>(
    token: &CancellationToken,
    mut attempt: F,
    retryable: impl Fn(&Result<T, E>) -> bool,
) -> Option<Result<T, E>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let mut delay = INITIAL_DELAY;
    for retried in 0..=MAX_RETRIES {
        if token.is_cancelled() {
            return None;
        }
        let outcome = tokio::select! {
            biased;
            _ = token.cancelled() => return None,
            outcome = attempt() => outcome,
        };
        if retried == MAX_RETRIES || !retryable(&outcome) {
            return Some(outcome);
        }
        // Release the failed response/connection before waiting.
        drop(outcome);
        tokio::select! {
            biased;
            _ = token.cancelled() => return None,
            _ = tokio::time::sleep(delay) => {},
        }
        delay = delay.saturating_mul(2).min(MAX_DELAY);
    }
    unreachable!("retry budget always produces a final outcome")
}

/// Preserve the existing Anthropic/Chat Completions error vocabulary.
/// Responses shares `send`, but keeps its own response/error normalization.
pub(super) async fn send_chat(
    client: &Client,
    request: RequestBuilder,
    url: &str,
    token: &CancellationToken,
) -> Result<Response, Value> {
    let outcome = match request.build() {
        Ok(request) => send(client, request, token).await,
        Err(error) => Err(SendError::Transport(error)),
    };
    match outcome {
        Ok(response) if response.status().is_success() => Ok(response),
        Ok(response) => {
            let status = response.status().as_u16();
            let detail = tokio::select! {
                biased;
                _ = token.cancelled() => return Err(json!({"status":"cancelled"})),
                detail = response.text() => detail.unwrap_or_default(),
            };
            Err(chat_http_failure(status, &detail))
        }
        Err(SendError::Cancelled) => Err(json!({"status":"cancelled"})),
        Err(error) => {
            let message = format!("could not reach {url}: {}", full_cause(&error));
            Err(json!({"status":"error", "error":error_info(
                "environment.network", &message, "environment", error.retryable(), error.retryable()
            )}))
        }
    }
}

fn chat_http_failure(status: u16, detail: &str) -> Value {
    let (code, blame, retryable) = match status {
        429 => ("provider.rate_limit", "provider", true),
        500..=599 => ("provider.unavailable", "provider", true),
        _ => ("provider.bad_request", "request", false),
    };
    let message = format!("API error {status}: {detail}");
    json!({"status":"error", "error":error_info(code, &message, blame, retryable, retryable)})
}

#[cfg(test)]
#[path = "model_http/tests.rs"]
mod tests;
