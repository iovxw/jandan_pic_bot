use std::time::Duration;
use std::sync::{Arc, Mutex};

use regex::Regex;
use curl::easy::Easy;
use tokio_curl::Session;
use serde_json;
use kuchiki;
use kuchiki::traits::*;
use futures::{self, Future, Stream};

use errors::*;

const JANDAN_HOME: &'static str = "http://jandan.net/";
const TUCAO_API: &'static str = "http://jandan.net/tucao/";

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
    tucao: Vec<Tucao>,
    has_next_page: bool,
}

#[derive(Deserialize, Debug)]
struct Tucao {
    #[serde(rename = "comment_ID")]
    comment_id: String,
    #[serde(rename = "comment_post_ID")]
    comment_post_id: String,
    comment_author: String,
    comment_date: String,
    comment_content: String,
    comment_parent: String,
    #[serde(rename = "comment_reply_ID")]
    comment_reply_id: String,
    vote_positive: String,
    vote_negative: String,
    comment_date_int: i64,
    is_tip_user: i64,
    is_jandan_user: i64,
}

fn escape_comment_content(s: &str) -> String {
    lazy_static!{
        static ref IMG: Regex =
            Regex::new(r#"<a href="(:?http|https:)?(?P<img>//[^"]*)"[^>]*>[^<]*</a><br><img[^>]*>"#)
            .unwrap();
        static ref AT: Regex = Regex::new(r#"<a[^>]*>(?P<at>[^<]*)</a>"#).unwrap();
    }
    let s = IMG.replace_all(s, "https:$img");
    let s = AT.replace_all(&s, "$at");

    s.replace("<br />\n", "\n")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn fix_scheme(s: String) -> String {
    if s.starts_with("//") {
        let mut ns = String::with_capacity(6 + s.len());
        ns.push_str("https:");
        ns.push_str(&s);
        ns
    } else {
        s
    }
}

fn make_request(url: &str) -> Result<(Easy, Arc<Mutex<Vec<u8>>>)> {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let buf2 = buf.clone();

    let mut client = Easy::new();
    client.url(url).unwrap();
    client.timeout(Duration::from_secs(10)).unwrap();
    client.follow_location(true).unwrap();
    client
        .write_function(move |data| {
            buf2.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        })
        .unwrap();
    client
        .useragent(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION"),
            " (+https://t.me/jandan_pic)"
        ))
        .unwrap();

    Ok((client, buf))
}

pub fn get_comments<'a>(
    session: &Session,
    id: &str,
) -> impl Future<Item = Vec<Comment>, Error = Error> + 'a {
    let url = format!("{}{}", TUCAO_API, id);

    let (request, body) = make_request(&url).unwrap();

    let req = session.perform(request);

    req.map_err(|e| e.into()).and_then(move |mut resp| {
        assert_eq!(resp.response_code().unwrap(), 200);
        let body = body.lock().unwrap();
        serde_json::from_slice::<TucaoResp>(&body)
            .map_err(|e| e.into())
            .and_then(|resp| {
                assert_eq!(resp.code, 0);
                resp.hot_tucao
                    .into_iter()
                    .map(|tucao| {
                        Ok(Comment {
                            author: tucao.comment_author,
                            oo: tucao.vote_positive.parse()?,
                            xx: tucao.vote_negative.parse()?,
                            content: escape_comment_content(&tucao.comment_content),
                        })
                    })
                    .collect::<Result<_>>()
            })
    })
}

#[inline]
fn image_name(link: &str) -> &str {
    link.split('/').last().unwrap_or("")
}

pub fn get_list<'a>(session: Session) -> impl Stream<Item = Pic, Error = Error> + 'a {
    let (request, body) = make_request(JANDAN_HOME).unwrap();

    let req = session.perform(request);

    req.map_err(|e| e.into())
        .and_then(move |mut resp| {
            assert_eq!(resp.response_code().unwrap(), 200);
            let body = body.lock().unwrap();
            let html = String::from_utf8_lossy(&body);

            let document = kuchiki::parse_html().one(&*html);

            document
                .select(
                    "#list-pic .acv_author, #list-pic .acv_comment, #list-pic .jandan-vote",
                )
                .unwrap()
                .collect::<Vec<_>>()
                .chunks(3)
                .map(|x| {
                    assert_eq!(x.len(), 3);
                    let acv_author = x[0].as_node();
                    let acv_comment = x[1].as_node();
                    let jandan_vote = x[2].as_node();

                    let author_raw = acv_author.first_child().unwrap();
                    let author = author_raw
                        .as_text()
                        .unwrap()
                        .borrow()
                        .split('@')
                        .next()
                        .ok_or("no author")?
                        .trim()
                        .to_owned();

                    let link_raw = acv_author.select("a[href]").map_err(|_| "")?.next().ok_or(
                        "no \"a[href]\" in \".acv_author\"",
                    )?;
                    let link = link_raw
                        .as_node()
                        .as_element()
                        .unwrap()
                        .attributes
                        .borrow()
                        .get("href")
                        .unwrap()
                        .to_owned();

                    let empty_line = Regex::new(r"^[\n\s]*$").unwrap();
                    let mut text = String::new();
                    for p in acv_comment.select("p").map_err(|_| "")? {
                        for node in p.as_node().children() {
                            if let Some(line) = node.as_text() {
                                let line = line.borrow();
                                if !empty_line.is_match(&line) {
                                    text.push_str(&line);
                                    text.push('\n');
                                }
                            }
                        }
                    }

                    let mut prev_name = String::new();
                    let images = acv_comment
                        .select("a.view_img_link[href], img[org_src], img[src]")
                        .unwrap()
                        .filter_map(|img| {
                            let attrs = img.as_node().as_element().unwrap().attributes.borrow();
                            let src = attrs
                                .get("href")
                                .or_else(|| attrs.get("org_src"))
                                .or_else(|| attrs.get("src"))
                                .ok_or("no org_src or src in \".acv_comment img\"");
                            if let Err(e) = src {
                                return Some(Err(e.into()));
                            }
                            let src = src.unwrap();
                            let name = image_name(src);
                            if prev_name != name {
                                prev_name.clear();
                                prev_name.push_str(name);
                                Some(Ok(fix_scheme(src.to_owned())))
                            } else {
                                None
                            }
                        })
                        .collect::<Result<Vec<_>>>()?;

                    let vote = jandan_vote
                        .select("span")
                        .unwrap()
                        .filter_map(|x| {
                            x.as_node().first_child().and_then(|x| {
                                x.as_text().map(|x| x.borrow().parse().unwrap_or(0))
                            })
                        })
                        .collect::<Vec<u32>>();

                    assert_eq!(vote.len(), 2);
                    let oo = vote[0];
                    let xx = vote[1];

                    let id = jandan_vote
                        .select("a[data-id]")
                        .unwrap()
                        .filter_map(|a| {
                            a.as_node()
                                .as_element()
                                .unwrap()
                                .attributes
                                .borrow()
                                .get("data-id")
                                .map(|x| x.to_owned())
                        })
                        .next()
                        .ok_or("no \"a[data-id]\" in \".jandan-vote\"")?;

                    Ok((author, link, id, oo, xx, text, images))
                })
                .collect::<Result<Vec<_>>>()
        })
        .map(move |index| {
            futures::stream::iter_ok(index).and_then(move |(author,
                   link,
                   id,
                   oo,
                   xx,
                   text,
                   images)| {
                get_comments(&session, &id).map(move |comments| {
                    Pic {
                        author,
                        link,
                        id,
                        oo,
                        xx,
                        text,
                        images,
                        comments,
                    }
                })
            })
        })
        .flatten_stream()
}
