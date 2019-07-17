use std::borrow::Cow;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use curl::easy::Easy;
use futures::{self, Future, Stream};
use kuchiki;
use kuchiki::traits::*;
use regex::Regex;
use serde_json;
use tokio_curl::Session;

use failure::{err_msg, Error};

const JANDAN_HOME: &str = "http://jandan.net/";
const TUCAO_API: &str = "http://jandan.net/tucao/";

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
    comment_author: String,
    comment_content: String,
    vote_positive: u32,
    vote_negative: u32,
}

pub fn escape_comment_content(s: &str) -> String {
    lazy_static!{
        static ref IMG: Regex = Regex::new(r#"<img src="(?P<img>[^"]+)" />"#).unwrap();
        static ref AT: Regex = Regex::new(r#"<a[^>]*>(?P<at>[^<]*)</a>"#).unwrap();
    }
    let s = IMG.replace_all(s, "$img");
    let s = AT.replace_all(&s, "$at");

    s.replace("<br />\n", "\n")
        .replace("&quot;", "\"")
        .replace("&#34;", "\"")
        .replace("&amp;", "&")
        .replace("&#38;", "&")
        .replace("&lt;", "<")
        .replace("&#60;", "<")
        .replace("&gt;", ">")
        .replace("&#62;", ">")
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

pub fn make_request(url: &str) -> Result<(Easy, Arc<Mutex<Vec<u8>>>), Error> {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let buf2 = Arc::clone(&buf);

    let mut client = Easy::new();
    client.url(url).unwrap();
    client.timeout(Duration::from_secs(60)).unwrap();
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

    let req = session.perform(request).map_err(|e| format_err!("{}", e));

    req.and_then(move |mut resp| {
        assert_eq!(resp.response_code().unwrap(), 200);
        let body = body.lock().unwrap();
        let resp = serde_json::from_slice::<TucaoResp>(&body)?;
        assert_eq!(resp.code, 0);
        resp.hot_tucao
            .into_iter()
            .map(|tucao| {
                Ok(Comment {
                    author: tucao.comment_author,
                    oo: tucao.vote_positive,
                    xx: tucao.vote_negative,
                    content: escape_comment_content(&tucao.comment_content),
                })
            })
            .collect::<Result<_, Error>>()
    })
}

pub fn get_list<'a>(session: Session) -> impl Stream<Item = Pic, Error = Error> + 'a {
    let (request, body) = make_request(JANDAN_HOME).unwrap();

    let req = session.perform(request).map_err(|e| format_err!("{}", e));

    req.and_then(move |mut resp| {
        assert_eq!(resp.response_code().unwrap(), 200);
        let body = body.lock().unwrap();
        let html = String::from_utf8_lossy(&body);

        let document = kuchiki::parse_html().one(&*html);

        document
            .select("#list-pic .acv_author, #list-pic .acv_comment, #list-pic .jandan-vote")
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
                    .ok_or(err_msg("no author"))?
                    .trim()
                    .to_owned();

                let link_raw = acv_author
                    .select("a[href]")
                    .map_err(|_| err_msg("no a[href]"))?
                    .next()
                    .ok_or(err_msg("no \"a[href]\" in \".acv_author\""))?;
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
                for p in acv_comment.select("p").map_err(|_| err_msg("no p"))? {
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

                let images = acv_comment
                    .select(".view_img_link")
                    .unwrap()
                    .map(|e| {
                        let src = e.as_node()
                            .as_element()
                            .unwrap()
                            .attributes
                            .borrow()
                            .get("href")
                            .unwrap()
                            .to_owned();
                        fix_scheme(&src).to_string()
                    })
                    .collect::<Vec<_>>();

                let vote = jandan_vote
                    .select("span")
                    .unwrap()
                    .filter_map(|x| {
                        x.as_node()
                         .first_child()
                         .and_then(|x| x.as_text().map(|x| x.borrow().parse().unwrap_or(0)))
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
                    .ok_or(err_msg("no \"a[data-id]\" in \".jandan-vote\""))?;

                Ok((author, link, id, oo, xx, text, images))
            })
            .collect::<Result<Vec<_>, Error>>()
    }).map(move |index| {
            futures::stream::iter_ok(index).and_then(
                move |(author, link, id, oo, xx, text, images)| {
                    get_comments(&session, &id).map(move |comments| Pic {
                        author,
                        link,
                        id,
                        oo,
                        xx,
                        text,
                        images,
                        comments,
                    })
                },
            )
        })
        .flatten_stream()
}

#[test]
fn test_to_large_img() {
    let r = to_large_img("//wx1.sinaimg.cn/mw600/93c0135dgy1fgjp10foprj20ti09b75u.jpg");
    assert_eq!(
        r,
        "//wx1.sinaimg.cn/large/93c0135dgy1fgjp10foprj20ti09b75u.jpg"
    );
}
