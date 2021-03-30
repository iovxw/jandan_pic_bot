use std::borrow::Cow;
use std::time::Duration;

use reqwest::header;
use serde::{Deserialize, Serialize, Serializer};

const WM_USER_STATUS: &str = "https://web.archive.org/save/status/user";
const WM_SAVE: &str = "https://web.archive.org/save";

#[derive(Deserialize)]
pub struct UserStatusResp {
    pub available: usize,
    pub daily_captures: usize,
    pub daily_captures_limit: usize,
    pub processing: usize,
}

#[derive(Serialize)]
pub struct SaveReq {
    pub url: String,
    #[serde(serialize_with = "ser_bool_as_int")]
    pub capture_all: bool,
    #[serde(serialize_with = "ser_bool_as_int")]
    pub capture_outlinks: bool,
    #[serde(serialize_with = "ser_bool_as_int")]
    pub force_get: bool,
    #[serde(serialize_with = "ser_bool_as_int")]
    pub skip_first_archive: bool,
}

pub async fn push(token: &str, imgs: &[Cow<'_, str>]) -> anyhow::Result<()> {
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::ACCEPT,
        header::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        header::AUTHORIZATION,
        format!("LOW {}", token).parse().unwrap(),
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .default_headers(headers)
        .build()
        .unwrap();

    for img in imgs {
        let status: UserStatusResp = client
            .get(WM_USER_STATUS)
            .query(&[(
                "_t",
                &std::time::Instant::now().elapsed().as_secs().to_string(),
            )])
            .send()
            .await?
            .json()
            .await?;

        assert!(status.daily_captures < status.daily_captures_limit);

        while status.available == 0 {
            tokio::time::delay_for(Duration::from_secs(5)).await;
        }

        let req = client
            .post(WM_SAVE)
            .form(&SaveReq {
                url: format!("https://jandan.net/t/{}", img),
                capture_all: true,
                capture_outlinks: false,
                force_get: true,
                skip_first_archive: true,
            }).build()?;
         dbg!(String::from_utf8_lossy(req.body().unwrap().as_bytes().unwrap()));

        client.execute(req)
            .await?;
    }
    Ok(())
}

fn ser_bool_as_int<S>(b: &bool, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.serialize_u8(if *b { 1 } else { 0 })
}
