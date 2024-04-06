use std::borrow::Cow;
use std::time::Duration;

use lazy_static::lazy_static;
use marksman_escape::Unescape;
use regex::Regex;
use reqwest::header;
use scraper::Html;
use serde::Deserialize;

const JANDAN_HOME: &str = "http://jandan.net/";
const TUCAO_API: &str = "http://jandan.net/tucao/";

thread_local! {
    pub static CLIENT: reqwest::Client = {
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

#[derive(Clone, Debug, PartialEq)]
pub struct Comment {
    pub author: String,
    pub oo: u32,
    pub xx: u32,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pic {
    pub author: String,
    pub link: String,
    pub id: String,
    pub oo: u32,
    pub xx: u32,
    pub text: String,
    pub images: Vec<String>,
    pub comments: Vec<Comment>,
}

#[derive(Deserialize, Debug)]
struct TucaoResp {
    code: i32,
    hot_tucao: Vec<Tucao>,
    #[allow(unused)]
    tucao: Vec<Tucao>,
    #[allow(unused)]
    has_next_page: bool,
}

#[derive(Deserialize, Debug)]
struct Tucao {
    comment_author: String,
    comment_content: String,
    vote_positive: u32,
    vote_negative: u32,
}

fn unescape_comment(s: &str) -> Cow<str> {
    lazy_static! {
        static ref RULES: [(Regex, &'static str); 3] = [
            (
                Regex::new(r#"<img src="(?P<img>[^"]+)" />"#).unwrap(),
                "$img"
            ),
            (Regex::new(r#"<a[^>]*>(?P<at>[^<]*)</a>"#).unwrap(), "$at"),
            (Regex::new("<br>").unwrap(), "\n")
        ];
    }

    let mut s = Cow::Borrowed(s.trim());
    for (r, rep) in RULES.iter() {
        // When a Cow::Borrowed is returned, the value returned is guaranteed
        // to be equivalent to the haystack given.
        if let Cow::Owned(ss) = r.replace_all(&s, *rep) {
            s = Cow::Owned(ss)
        }
    }
    s
}

fn fix_scheme(s: &str) -> Cow<str> {
    if s.starts_with("//") {
        let mut ns = String::with_capacity(6 + s.len());
        ns.push_str("https:");
        ns.push_str(&s);
        Cow::Owned(ns)
    } else {
        Cow::Borrowed(s)
    }
}

async fn get_comments(id: &str) -> anyhow::Result<Vec<Comment>> {
    let url = format!("{}{}", TUCAO_API, id);

    let resp = CLIENT
        .with(|client| client.get(&url))
        .send()
        .await?
        .error_for_status()?
        .json::<TucaoResp>()
        .await?;
    assert_eq!(resp.code, 0);

    resp.hot_tucao
        .into_iter()
        .map(|tucao| {
            Ok(Comment {
                author: tucao.comment_author,
                oo: tucao.vote_positive,
                xx: tucao.vote_negative,
                content: unescape_comment(&tucao.comment_content).into_owned(),
            })
        })
        .collect::<Result<_, _>>()
}

macro_rules! pos {
    () => {
        concat!(file!(), ": ", line!(), ",", column!())
    };
}

mod selector {
    use lazy_static::lazy_static;
    use scraper::Selector;
    lazy_static! {
        pub static ref AUTHOR: Selector = Selector::parse("#list-pic .acv_author").unwrap();
        pub static ref COMMENT: Selector = Selector::parse("#list-pic .acv_comment").unwrap();
        pub static ref COMMENT_IMG: Selector = Selector::parse(".view_img_link").unwrap();
        pub static ref VOTE: Selector = Selector::parse("#list-pic .jandan-vote").unwrap();
        pub static ref ID: Selector = Selector::parse("a[data-id]").unwrap();
        pub static ref HREF: Selector = Selector::parse("*[href]").unwrap();
        pub static ref P: Selector = Selector::parse("p").unwrap();
        pub static ref SPAN: Selector = Selector::parse("span").unwrap();
    }
}

pub async fn do_the_evil() -> anyhow::Result<Vec<Pic>> {
    let html = CLIENT
        .with(|client| client.get(JANDAN_HOME))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    let document = Html::parse_document(&html);

    let mut pics = Vec::new();

    for ((author_div, comment_div), vote_div) in document
        .select(&selector::AUTHOR)
        .zip(document.select(&selector::COMMENT))
        .zip(document.select(&selector::VOTE))
    {
        let author = author_div
            .text()
            .next()
            .expect(pos!())
            .split('@')
            .next()
            .expect(pos!())
            .trim()
            .to_owned();
        let link = author_div
            .select(&selector::HREF)
            .next()
            .expect(pos!())
            .value()
            .attr("href")
            .expect(pos!())
            .to_owned();
        let text_buf = comment_div
            .select(&selector::P)
            .flat_map(|p| p.children())
            .filter_map(|child| child.value().as_text())
            .map(|text| text.text.trim_matches('\n'))
            .filter(|line| !line.is_empty())
            .intersperse("\n")
            .map(|line| Unescape::new(line.as_bytes().iter().copied()))
            .flatten()
            .collect::<Vec<u8>>();
        let text = String::from_utf8(text_buf).unwrap();
        let images = comment_div
            .select(&selector::COMMENT_IMG)
            .map(|a| a.value().attr("href").expect(pos!()))
            .map(|href| fix_scheme(href).into_owned())
            .collect::<Vec<String>>();
        let mut votes = vote_div
            .select(&selector::SPAN)
            .map(|span| span.text().next().expect(pos!()))
            .map(|vote_str| vote_str.parse::<u32>().expect(pos!()));
        let (oo, xx) = (votes.next().expect(pos!()), votes.next().expect(pos!()));
        let id = vote_div
            .select(&selector::ID)
            .next()
            .expect(pos!())
            .value()
            .attr("data-id")
            .expect(pos!())
            .to_string();
        let comments = get_comments(&id).await?;
        let pic = Pic {
            author,
            link,
            id,
            oo,
            xx,
            text,
            images,
            comments,
        };
        pics.push(pic);
    }

    Ok(pics)
}

#[cfg(test)]
mod test {
    use super::*;
    #[tokio::test]
    #[ignore]
    async fn test() {
        dbg!(do_the_evil().await.unwrap());
    }

    #[test]
    fn unescape() {
        let s = r##"<a href=\"#tucao-6023158\" data-id=\"6023158\" class=\"tucao-link\">@name</a> COMMENT"##;
        let r = unescape_comment(s);
        assert_eq!(&*r, "@name COMMENT")
    }
}
