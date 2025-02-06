use std::collections::HashMap;
use std::ops::Range;
use std::time::Duration;
use std::{borrow::Cow, collections::BTreeMap};

use lazy_static::lazy_static;
use marksman_escape::Unescape;
use regex::Regex;
use reqwest::header;
use scraper::{Html, Selector};
use serde::Deserialize;

const JANDAN_HOME: &str = "http://jandan.net";
const JANDAN_THREAD: &str = "http://jandan.net/t/";
const TUCAO_API: &str = "http://jandan.net/api/tucao/list/";

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
    pub id: u64,
    pub author: String,
    pub oo: u32,
    pub xx: u32,
    pub content: RichText,
    pub mentions: Vec<u64>,
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
    pub comments: Comments,
}

#[derive(Deserialize)]
struct RawPic<'a> {
    id: u64,
    author: String,
    #[serde(borrow)]
    content: Cow<'a, str>,
    vote_positive: u32,
    vote_negative: u32,
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
    #[serde(rename = "comment_ID")]
    comment_id: u64,
    comment_author: String,
    #[serde(deserialize_with = "deserialize_comment_with_unescape")]
    comment_content: String,
    vote_positive: u32,
    vote_negative: u32,
}

fn deserialize_comment_with_unescape<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // https://github.com/serde-rs/serde/issues/1852
    #[derive(Deserialize)]
    struct BorrowCow<'a>(#[serde(borrow)] Cow<'a, str>);
    let s = BorrowCow::deserialize(deserializer)?.0;
    String::from_utf8(Unescape::new(s.bytes()).collect::<Vec<u8>>())
        .map_err(|e| serde::de::Error::custom(e))
}

#[derive(Debug, Clone, PartialEq)]
enum EntityRange {
    Text {
        range: Range<usize>,
    },
    Img {
        range: Range<usize>,
        url: Range<usize>,
    },
    Mention {
        range: Range<usize>,
        name: Range<usize>,
        id: u64,
    },
    Br {
        range: Range<usize>,
    },
}
impl EntityRange {
    fn range(&self) -> Range<usize> {
        use EntityRange::*;
        match self {
            Text { range } | Img { range, .. } | Mention { range, .. } | Br { range } => {
                range.clone()
            }
        }
    }
    fn to_text_entity<'a>(&'a self, s: &'a str) -> Option<TextEntity<'a>> {
        use EntityRange::*;
        match self {
            Text { range, .. } => s.get(range.clone()).map(TextEntity::Text),
            Img { url, .. } => s.get(url.clone()).map(TextEntity::Img),
            Mention { name, id, .. } => s
                .get(name.clone())
                .map(|name| TextEntity::Mention { name, id: *id }),
            Br { range } => s.get(range.clone()).map(|_| TextEntity::Br),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RichText {
    s: String,
    entities: Vec<EntityRange>,
}
impl RichText {
    pub fn entities<'a>(&'a self) -> impl Iterator<Item = TextEntity<'a>> {
        self.entities
            .iter()
            .map(|range| range.to_text_entity(&self.s).expect(""))
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum TextEntity<'a> {
    Text(&'a str),
    Img(&'a str),
    Mention { name: &'a str, id: u64 },
    Br,
}

fn parse_comment(s: String) -> RichText {
    lazy_static! {
        static ref RULES: [(Regex, fn(m: regex::Captures) -> EntityRange); 3] = [
            (
                Regex::new(r#"<img src="(?P<img>[^"]+)" />"#).unwrap(),
                |c| -> EntityRange {
                    EntityRange::Img {
                        range: c.get(0).unwrap().range(),
                        url: c.name("img").expect("missing 'img' in regex").range(),
                    }
                }
            ),
            (
                Regex::new(r#"<a .*data-id="(?P<id>\d+)".*>(?P<at>[^<]*)</a>"#).unwrap(),
                |c| {
                    EntityRange::Mention {
                        range: c.get(0).unwrap().range(),
                        name: c.name("at").expect("missing 'at' in regex").range(),
                        id: c
                            .name("id")
                            .expect("missing 'id' in regex")
                            .as_str()
                            .parse()
                            .expect("data-id format changed"),
                    }
                }
            ),
            (Regex::new("<br>").unwrap(), |c| {
                EntityRange::Br {
                    range: c.get(0).unwrap().range(),
                }
            })
        ];
    }

    let mut entities: Vec<EntityRange> = RULES
        .iter()
        .map(|(reg, f)| reg.captures_iter(&s).map(f))
        .flatten()
        .collect();
    entities.sort_by_key(|e| e.range().start);
    let len_freezed: i128 = entities.len().try_into().expect("overflow");
    for i in -1..len_freezed {
        let start = if i == -1 {
            0 // start of the string
        } else {
            entities[i as usize].range().end
        };
        let end = if i + 1 < len_freezed {
            entities[(i + 1) as usize].range().start
        } else {
            s.len() // end of the string
        };
        assert!(start <= end, "overlap");
        if start == end {
            continue;
        }
        entities.push(EntityRange::Text {
            range: Range { start, end },
        })
    }
    entities.sort_by_key(|e| e.range().start);
    RichText { s, entities }
}

fn extract_mentions(comment: &str) -> Vec<u64> {
    lazy_static! {
        // <a href="#tucao-12116426" data-id="12116426" class="tucao-link">
        static ref MENTIONS: Regex = Regex::new(r#"<a .*data-id="(?P<id>\d+)".*>"#).unwrap();
    }
    MENTIONS
        .captures_iter(comment)
        .map(|c| c.name("id").expect("bug in regex").as_str())
        .map(|id| id.parse::<u64>().expect("tucao ID format changed"))
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Comments {
    pub hot: Vec<Comment>,
    pub mentions: BTreeMap<u64, Option<Comment>>,
}

impl From<Tucao> for Comment {
    fn from(tucao: Tucao) -> Self {
        let mentions = extract_mentions(&tucao.comment_content);
        Comment {
            id: tucao.comment_id,
            author: tucao.comment_author,
            oo: tucao.vote_positive,
            xx: tucao.vote_negative,
            content: parse_comment(tucao.comment_content),
            mentions,
        }
    }
}

async fn get_comments(id: u64) -> anyhow::Result<Comments> {
    let url = format!("{}{}", TUCAO_API, id);

    let resp = CLIENT
        .with(|client| client.get(&url))
        .send()
        .await?
        .error_for_status()?
        .json::<TucaoResp>()
        .await?;
    assert_eq!(resp.code, 0);

    let hot: Vec<Comment> = resp.hot_tucao.into_iter().map(|c| c.into()).collect();

    let mut tucao: HashMap<u64, Tucao> =
        HashMap::from_iter(resp.tucao.into_iter().map(|c| (c.comment_id, c)));

    let mut mentions: BTreeMap<u64, Option<Comment>> = BTreeMap::new();
    let mut id_stack: Vec<_> = hot
        .iter()
        .map(|c| c.mentions.iter().map(|x| *x))
        .flatten()
        .collect();
    while let Some(id) = id_stack.pop() {
        if let Some(t) = tucao.remove(&id) {
            let c: Comment = t.into();
            id_stack.extend_from_slice(&c.mentions);
            mentions.insert(c.id, Some(c));
        } else if !mentions.contains_key(&id) {
            mentions.insert(id, None);
        }
    }
    Ok(Comments { hot, mentions })
}

macro_rules! regex {
    ($rule:literal) => {{
        use std::sync::LazyLock;
        static R: LazyLock<Regex> = LazyLock::new(|| Regex::new($rule).unwrap());
        &*R
    }};
}

pub async fn do_the_evil() -> anyhow::Result<Vec<Pic>> {
    let html = CLIENT
        .with(|client| client.get(JANDAN_HOME))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    let url = &regex!(r#"load_sidebar_list\('([^']+)"#)
        .captures(&html)
        .expect("load_sidebar_list not found")[1];

    let resp = CLIENT
        .with(|client| client.get(&format!("{}{}", JANDAN_HOME, url)))
        .send()
        .await?
        .error_for_status()?
        .bytes() // .json doesn't support borrowed deserialize
        .await?;

    #[derive(Deserialize)]
    struct Resp<'a> {
        #[serde(borrow)]
        data: Vec<RawPic<'a>>,
    }
    let resp: Resp = serde_json::from_slice(&resp)?;

    let mut pics = Vec::new();

    for raw_pic in resp.data {
        let content = Html::parse_fragment(&raw_pic.content);
        let text: String = content.root_element().text().collect();
        let text = regex!("\n+").replace_all(&text, "\n").trim().to_owned();
        let images = content
            .select(&Selector::parse("img").unwrap())
            .filter_map(|e| e.attr("src"))
            .map(str::to_owned)
            .collect();
        let comments = get_comments(raw_pic.id).await?;
        pics.push(Pic {
            author: raw_pic.author,
            link: format!("{}{}", JANDAN_THREAD, raw_pic.id),
            oo: raw_pic.vote_positive,
            xx: raw_pic.vote_negative,
            id: raw_pic.id.to_string(),
            text,
            images,
            comments,
        });
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
    fn rich_text() {
        let s = r##"<a href="#tucao-123" data-id="123" class="tucao-link">@name</a> COMMENT <img src="link" /><br>"##;
        let r = parse_comment(s.to_string());
        let r = r.entities().collect::<Vec<_>>();
        use TextEntity::*;
        assert_eq!(
            r,
            vec![
                Mention {
                    name: "@name",
                    id: 123
                },
                Text(" COMMENT ",),
                Img("link",),
                Br
            ]
        )
    }
}
