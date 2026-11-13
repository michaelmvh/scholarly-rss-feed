//! Shared rules for which upstream HTTP failures are worth asking again about.
//! Used by the OpenAlex and bioRxiv fetchers so one brief outage doesn't abort a whole run.

use std::time::Duration;

/// How many times a failed page is asked for again before the run gives up.
pub const MAX_RETRIES: u32 = 2;

/// Wait before the first retry; each later retry waits twice as long (15 s, then 30 s).
/// Upstream outages usually clear within a minute. Tests use a tiny delay to stay fast.
pub const FIRST_RETRY_DELAY: Duration = if cfg!(test) {
    Duration::from_millis(10)
} else {
    Duration::from_secs(15)
};

pub fn retry_delay(first_retry_delay: Duration, retry: u32) -> Duration {
    first_retry_delay * 2_u32.pow(retry)
}

/// 502/503/504 mean a gateway or overloaded server, not a problem with the request itself.
pub fn is_temporary_server_status(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::BAD_GATEWAY
            | reqwest::StatusCode::SERVICE_UNAVAILABLE
            | reqwest::StatusCode::GATEWAY_TIMEOUT
    )
}

/// No response at all: the connection failed or the server took longer than the timeout.
pub fn is_temporary_send_failure(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect()
}
