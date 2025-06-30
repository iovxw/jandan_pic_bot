use std::time::Duration;

use reqwest::header;

thread_local! {
    static CLIENT: reqwest::Client = {
        let headers = header::HeaderMap::new();
        const USER_AGENT: &str = concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION"),
            " (+https://t.me/jandan_pic)"
        );
        reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .user_agent(header::HeaderValue::from_static(USER_AGENT))
            .default_headers(headers)
            .build()
            .unwrap()
    }
}

async fn request<F: Fn(&reqwest::Client) -> reqwest::RequestBuilder>(
    build_request: F,
) -> reqwest::Result<reqwest::Response> {
    for attempt in (0..3).rev() {
        match CLIENT.with(|client| build_request(client)).send().await {
            Ok(resp) => return Ok(resp),
            Err(e) if e.is_timeout() && attempt > 0 => {
                log::warn!(
                    "Request timed out ({:?}), retrying... ({} attempts left)",
                    e.url(),
                    attempt
                );
                tokio::time::delay_for(Duration::from_secs(1)).await;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

pub async fn get(url: &str) -> reqwest::Result<reqwest::Response> {
    request(|client| client.get(url)).await
}

pub async fn get_with_referer(url: &str, referer: &str) -> reqwest::Result<reqwest::Response> {
    request(|client| client.get(url).header(header::REFERER, referer)).await
}
