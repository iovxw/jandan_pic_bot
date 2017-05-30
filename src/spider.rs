use std::time::Duration;

use regex::Regex;
use curl::easy::Easy;
use serde_json;
use kuchiki;
use kuchiki::traits::*;

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

fn escape_comment_content(s: String) -> String {
    Regex::new(r"<a[^>]*>(?P<at>[^<]*)</a>")
        .unwrap()
        .replace_all(&s, "$at")
        .replace("<br />\n", "\n")
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

pub fn get_comments(id: &str) -> Result<Vec<Comment>> {
    let url = format!("{}{}", TUCAO_API, id);

    let mut buf: Vec<u8> = Vec::new();

    let mut client = Easy::new();
    client.url(&url).unwrap();
    client.timeout(Duration::from_secs(10)).unwrap();
    client.follow_location(true).unwrap();
    {
        let mut transfer = client.transfer();
        transfer
            .write_function(|data| {
                                buf.extend_from_slice(data);
                                Ok(data.len())
                            })
            .unwrap();
        try!(transfer.perform());
    }

    serde_json::from_slice::<TucaoResp>(&buf)
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
                                content: escape_comment_content(tucao.comment_content),
                            })
                     })
                .collect::<Result<_>>()
        })
}

#[inline]
fn image_name(link: &str) -> &str {
    link.split('/').last().unwrap_or("")
}

pub fn get_list() -> Result<Vec<Pic>> {
    let mut buf: Vec<u8> = Vec::new();

    let mut client = Easy::new();
    client.url(JANDAN_HOME).unwrap();
    client.timeout(Duration::from_secs(10)).unwrap();
    client.follow_location(true).unwrap();
    // jandan.net's certificate is invalid (CN is *.jandan.net), ignore it
    client.ssl_verify_peer(false).unwrap();
    {
        let mut transfer = client.transfer();
        transfer
            .write_function(|data| {
                                buf.extend_from_slice(data);
                                Ok(data.len())
                            })
            .unwrap();
        try!(transfer.perform());
    }

    let html = String::from_utf8(buf)
        .chain_err(|| "response is not UTF-8")?;

    let document = kuchiki::parse_html().one(html);

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
                .ok_or("no author")?
                .trim()
                .to_owned();

            let link_raw = acv_author
                .select("a[href]")
                .map_err(|_| "")?
                .next()
                .ok_or("no \"a[href]\" in \".acv_author\"")?;
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
                    let name = image_name(&src);
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
                .ok_or("no \"a[data-id]\" in \".jandan-vote\"")?;

            let comments = try!(get_comments(&id));

            Ok(Pic {
                   author,
                   link,
                   id,
                   oo,
                   xx,
                   text,
                   images,
                   comments,
               })
        })
        .collect::<Result<Vec<Pic>>>()
}
