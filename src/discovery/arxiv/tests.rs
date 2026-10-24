use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

fn response_server(
    responses: Vec<(u16, &'static str)>,
) -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, headers) in responses {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(std::time::Instant::now() < deadline, "request timed out");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                assert!(request.len() < 8192);
            }
            requests.push(String::from_utf8(request).unwrap());
            write!(
                stream,
                "HTTP/1.1 {status} Test Response\r\n{headers}Content-Length: 7\r\nConnection: close\r\n\r\nfixture"
            )
            .unwrap();
        }
        requests
    });
    (address, handle)
}

#[tokio::test]
async fn rss_and_atom_requests_share_the_rate_limit() {
    let (address, server) = response_server(vec![(200, ""), (200, "")]);
    let client = reqwest::Client::new();
    let started = Instant::now();
    fetch_bytes(client.get(format!("{address}/rss")), "arXiv RSS")
        .await
        .unwrap();
    fetch_bytes(client.get(format!("{address}/query")), "arXiv Atom API")
        .await
        .unwrap();
    assert!(started.elapsed() >= API_DELAY);
    let requests = server.join().unwrap();
    assert!(requests[0].starts_with("GET /rss "));
    assert!(requests[1].starts_with("GET /query "));
}

#[test]
fn retry_after_seconds_and_http_dates_are_honored() {
    let future = httpdate::parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT").unwrap();
    let now = future - Duration::from_secs(60);
    for value in [
        "60",
        "Sun, 06 Nov 1994 08:49:37 GMT",
        "Sunday, 06-Nov-94 08:49:37 GMT",
        "Sun Nov  6 08:49:37 1994",
    ] {
        let header = reqwest::header::HeaderValue::from_str(value).unwrap();
        assert_eq!(
            retry_delay(Some(&header), 0, Duration::ZERO, now).unwrap(),
            Duration::from_secs(60),
            "{value}"
        );
    }
    for value in ["0", "1", "Sun, 06 Nov 1994 08:49:37 GMT"] {
        let header = reqwest::header::HeaderValue::from_str(value).unwrap();
        assert_eq!(
            retry_delay(Some(&header), 0, Duration::ZERO, future).unwrap(),
            API_DELAY
        );
    }
}

#[test]
fn missing_retry_after_uses_bounded_exponential_backoff() {
    let mut waited = Duration::ZERO;
    for (retry, seconds) in [30, 60, 120].into_iter().enumerate() {
        let delay = retry_delay(None, retry as u32, waited, SystemTime::now()).unwrap();
        assert_eq!(delay, Duration::from_secs(seconds));
        waited += delay;
    }
    assert_eq!(waited, Duration::from_secs(210));
    assert!(waited <= MAX_RETRY_WAIT);
}

#[test]
fn invalid_or_excessive_retry_after_never_causes_an_early_retry() {
    let now = SystemTime::now();
    for value in ["", "-1", "+1", "not a date", "18446744073709551616"] {
        let header = reqwest::header::HeaderValue::from_str(value).unwrap();
        assert!(retry_delay(Some(&header), 0, Duration::ZERO, now)
            .unwrap_err()
            .contains("Invalid Retry-After"));
    }
    let non_text = reqwest::header::HeaderValue::from_bytes(b"\xff").unwrap();
    assert!(retry_delay(Some(&non_text), 0, Duration::ZERO, now).is_err());
    for value in ["301", "18446744073709551615"] {
        let header = reqwest::header::HeaderValue::from_str(value).unwrap();
        assert!(retry_delay(Some(&header), 0, Duration::ZERO, now)
            .unwrap_err()
            .contains("no retry sent"));
    }
    let header = reqwest::header::HeaderValue::from_static("120");
    assert_eq!(
        retry_delay(Some(&header), 2, Duration::from_secs(180), now).unwrap(),
        Duration::from_secs(120)
    );
    assert!(retry_delay(Some(&header), 2, Duration::from_secs(181), now).is_err());
}

#[tokio::test]
async fn retries_429_with_the_same_query_after_the_requested_delay() {
    let (address, server) =
        response_server(vec![(429, "Retry-After: 4\r\n"), (200, "")]);
    let limiter = tokio::sync::Mutex::new(None);
    let started = Instant::now();
    let bytes = fetch_bytes_with_limiter(
        reqwest::Client::new().get(format!("{address}/query?start=0&max_results=2000")),
        "arXiv Atom API",
        &limiter,
    )
    .await
    .unwrap();
    assert_eq!(bytes, b"fixture");
    assert!(started.elapsed() >= Duration::from_secs(4));
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], requests[1]);
}

#[tokio::test]
async fn exhausted_429_retries_still_fail_required_refresh() {
    let (address, server) = response_server(vec![(429, "Retry-After: 0\r\n"); 4]);
    let limiter = tokio::sync::Mutex::new(None);
    let started = Instant::now();
    let error = fetch_bytes_with_limiter(
        reqwest::Client::new().get(address),
        "arXiv RSS",
        &limiter,
    )
    .await
    .unwrap_err();
    assert!(error.contains("arXiv RSS rate limited (HTTP 429) after 4 attempts"));
    assert!(started.elapsed() >= API_DELAY * MAX_RATE_LIMIT_RETRIES);
    assert_eq!(server.join().unwrap().len(), 4);
    assert!(RefreshPolicy::RequireFresh.check_failure(&error).is_err());
    assert!(RefreshPolicy::AllowStale.check_failure(&error).is_ok());
}

#[tokio::test]
async fn excessive_server_cooldown_and_other_status_errors_are_not_retried() {
    for (status, headers, expected) in [
        (429, "Retry-After: 301\r\n", "required cooldown of 301.0s"),
        (429, "Retry-After: invalid\r\n", "Invalid Retry-After"),
        (403, "Retry-After: 0\r\n", "403"),
        (503, "Retry-After: 0\r\n", "503"),
    ] {
        let (address, server) = response_server(vec![(status, headers)]);
        let limiter = tokio::sync::Mutex::new(None);
        let error = fetch_bytes_with_limiter(
            reqwest::Client::new().get(address),
            "arXiv Atom API",
            &limiter,
        )
        .await
        .unwrap_err();
        assert!(error.contains(expected), "{error}");
        assert_eq!(server.join().unwrap().len(), 1);
    }
}

#[test]
fn validates_only_pilot_categories() {
    assert!(validate_categories(&["q-bio.BM".to_string(), "q-bio.QM".to_string()]).is_ok());
    assert!(validate_categories(&[
        "q-bio.GN".to_string(),
        "q-bio.MN".to_string(),
        "q-bio.PE".to_string(),
        "physics.bio-ph".to_string(),
    ])
    .is_ok());
    assert_eq!(
        validate_categories(&["cs.LG".to_string()]).unwrap_err(),
        "Unknown arXiv category \"cs.LG\"."
    );
}

#[test]
fn builds_bounded_atom_query() {
    assert_eq!(
        atom_query(
            &["q-bio.BM".to_string(), "q-bio.QM".to_string()],
            "2026-08-01",
            "2026-09-01"
        )
        .unwrap(),
        "(cat:q-bio.BM OR cat:q-bio.QM) AND submittedDate:[202608010000 TO 202609012359]"
    );
}

#[test]
fn parses_modern_and_legacy_arxiv_ids() {
    assert_eq!(
        parse_arxiv_id("https://arxiv.org/abs/2608.30337v2").as_deref(),
        Some("2608.30337")
    );
    assert_eq!(
        parse_arxiv_id("oai:arXiv.org:q-bio/0601001v1").as_deref(),
        Some("q-bio/0601001")
    );
}

#[test]
fn derives_month_date_from_arxiv_identifiers() {
    assert_eq!(
        arxiv_month_date("2608.30337").as_deref(),
        Some("2026-08-01")
    );
    assert_eq!(
        arxiv_month_date("q-bio/0601001").as_deref(),
        Some("2006-01-01")
    );
}

#[test]
fn builds_arxiv_work_with_matching_category_provenance() {
    let categories = vec!["cs.LG".to_string(), "q-bio.BM".to_string()];
    let configured = vec!["q-bio.BM".to_string(), "q-bio.QM".to_string()];

    let work = build_work(
        "2608.30337",
        "  A\n title ",
        Some(" An abstract. "),
        ["Ada Lovelace"],
        "2026-08-31",
        "2026-09-01",
        &categories,
        &configured,
        "https://arxiv.org/abs/2608.30337".to_string(),
        "https://arxiv.org/pdf/2608.30337".to_string(),
        Some("http://creativecommons.org/licenses/by/4.0/"),
        Some("10.1000/published"),
    );

    assert_eq!(work.title.as_deref(), Some("A title"));
    assert_eq!(
        work.doi.as_deref(),
        Some("https://doi.org/10.48550/arXiv.2608.30337")
    );
    assert_eq!(work.author_names(), vec!["Ada Lovelace"]);
    assert_eq!(work.abstract_text().as_deref(), Some("An abstract."));
    assert_eq!(work.published_doi.as_deref(), Some("10.1000/published"));
    assert_eq!(
        work.discovery_sources,
        vec![DiscoverySource::arxiv_category("q-bio.BM", "Biomolecules")]
    );
}

#[test]
fn computes_process_wide_api_delay() {
    let now = Instant::now();

    assert_eq!(remaining_api_delay(None, now), None);
    assert_eq!(
        remaining_api_delay(Some(now - Duration::from_secs(1)), now),
        Some(Duration::from_secs(2))
    );
    assert_eq!(
        remaining_api_delay(Some(now - Duration::from_secs(3)), now),
        None
    );
}
