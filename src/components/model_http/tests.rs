use super::*;
use std::cell::{Cell, RefCell};
use tokio::time::Instant;

fn can_retry(outcome: &Result<StatusCode, usize>) -> bool {
    match outcome {
        Ok(status) => retryable_status(*status),
        Err(_) => true,
    }
}

#[tokio::test(start_paused = true)]
async fn ten_retries_double_the_wait_then_cap_at_thirty_seconds() {
    let token = CancellationToken::new();
    let starts = RefCell::new(Vec::new());
    let result = retry(
        &token,
        || async {
            let mut starts = starts.borrow_mut();
            starts.push(Instant::now());
            Err::<StatusCode, _>(starts.len())
        },
        can_retry,
    )
    .await;
    assert_eq!(result, Some(Err(11)), "the initial request is not a retry");
    let starts = starts.into_inner();
    let waits: Vec<_> = starts
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).as_millis())
        .collect();
    assert_eq!(
        waits,
        [200, 400, 800, 1600, 3200, 6400, 12800, 25600, 30000, 30000]
    );
    assert_eq!(
        Instant::now() - starts[0],
        Duration::from_secs(111),
        "no wait after the final attempt"
    );
}

#[tokio::test(start_paused = true)]
async fn only_rate_limits_and_server_statuses_retry_and_final_errors_keep_their_meaning() {
    for code in [
        200, 204, 302, 400, 401, 403, 404, 408, 422, 429, 500, 503, 529, 599,
    ] {
        let status = StatusCode::from_u16(code).unwrap();
        let calls = Cell::new(0);
        let result = retry(
            &CancellationToken::new(),
            || async {
                calls.set(calls.get() + 1);
                Ok(status)
            },
            can_retry,
        )
        .await;
        assert_eq!(result, Some(Ok(status)));
        let retryable = code == 429 || code >= 500;
        assert_eq!(calls.get(), if retryable { 11 } else { 1 }, "HTTP {code}");
        if code >= 400 {
            let payload = chat_http_failure(code, "fixture failure");
            assert_eq!(payload["error"]["retryable"], retryable);
            assert_eq!(payload["error"]["transient"], retryable);
            assert_eq!(
                payload["error"]["blame"],
                if retryable { "provider" } else { "request" }
            );
            assert_eq!(
                payload["error"]["code"],
                match code {
                    429 => "provider.rate_limit",
                    500..=599 => "provider.unavailable",
                    _ => "provider.bad_request",
                }
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_success_on_the_last_retry_is_returned_without_an_extra_wait() {
    let calls = Cell::new(0);
    let started = Instant::now();
    let result = retry(
        &CancellationToken::new(),
        || async {
            calls.set(calls.get() + 1);
            if calls.get() < 11 {
                Err(calls.get())
            } else {
                Ok(StatusCode::OK)
            }
        },
        can_retry,
    )
    .await;
    assert_eq!(result, Some(Ok(StatusCode::OK)));
    assert_eq!(calls.get(), 11);
    assert_eq!(Instant::now() - started, Duration::from_secs(111));
}

#[tokio::test(start_paused = true)]
async fn cancellation_before_sending_does_not_start_an_attempt() {
    let token = CancellationToken::new();
    token.cancel();
    let result = retry(
        &token,
        || async { panic!("cancelled request was sent") },
        can_retry,
    )
    .await;
    assert!(result.is_none());
}

#[tokio::test(start_paused = true)]
async fn cancellation_interrupts_an_active_send() {
    let token = CancellationToken::new();
    let calls = Cell::new(0);
    let work = retry(
        &token,
        || async {
            calls.set(calls.get() + 1);
            std::future::pending::<Result<StatusCode, usize>>().await
        },
        can_retry,
    );
    tokio::pin!(work);
    assert!(futures_util::poll!(&mut work).is_pending());
    assert_eq!(calls.get(), 1);
    token.cancel();
    assert!(work.await.is_none());
    assert_eq!(calls.get(), 1);
}

#[tokio::test(start_paused = true)]
async fn cancellation_during_backoff_wins_even_when_the_timer_is_already_ready() {
    for timer_ready in [false, true] {
        let token = CancellationToken::new();
        let calls = Cell::new(0);
        let work = retry(
            &token,
            || async {
                calls.set(calls.get() + 1);
                Err(calls.get())
            },
            can_retry,
        );
        tokio::pin!(work);
        assert!(futures_util::poll!(&mut work).is_pending());
        assert_eq!(calls.get(), 1);
        if timer_ready {
            tokio::time::advance(Duration::from_millis(200)).await;
        }
        token.cancel();
        assert!(work.await.is_none());
        tokio::time::advance(Duration::from_secs(30)).await;
        assert_eq!(
            calls.get(),
            1,
            "cancellation must not launch another request"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_non_replayable_body_and_invalid_request_fail_without_waiting() {
    let client = Client::new();
    let body = reqwest::Body::wrap_stream(futures_util::stream::pending::<
        Result<Vec<u8>, std::io::Error>,
    >());
    let request = client
        .post("http://127.0.0.1:1")
        .body(body)
        .build()
        .unwrap();
    let started = Instant::now();
    let error = send(&client, request, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(error, SendError::NotReplayable));
    assert!(!error.retryable());
    let error = SendError::Transport(client.post("http://[invalid").build().unwrap_err());
    assert!(!error.retryable());
    let result = send_chat(
        &client,
        client.post("http://[invalid"),
        "http://[invalid",
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(result["error"]["retryable"], false);
    assert_eq!(Instant::now(), started);
}
