use std::io;
use std::time::Duration;

use regex::Regex;

use curl::easy::Easy;

use serde_json;

use select::document::Document;
use select::node::Data;
use select::predicate::{Predicate, Attr, Class, Name};

use errors::*;

const JANDAN_HOME: &'static str = "https://jandan.net/";
const TUCAO_API: &'static str = "http://jandan.net/tucao/";

#[derive(Clone, Debug, PartialEq)]
pub struct Comment {
    pub author: String,
    pub xx: u32,
    pub oo: u32,
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

    let buf = io::Cursor::new(buf);

    let document = try!(Document::from_read(buf));

    document
        .find(Attr("id", "list-pic"))
        .next()
        .expect("can not find #list-pic")
        .find(Class("acv_author")
                  .or(Class("acv_comment"))
                  .or(Class("jandan-vote")))
        .collect::<Vec<_>>()
        .chunks(3)
        .map(|x| {
            let acv_author = x[0];
            let acv_comment = x[1];
            let jandan_vote = x[2];

            let author = Regex::new(r"^[^\s@]+")
                .unwrap()
                .captures(&acv_author.first_child().unwrap().text())
                .unwrap()
                .at(0)
                .expect("no author")
                .to_string();

            let link = acv_author
                .find(Name("a"))
                .next()
                .unwrap()
                .attr("href")
                .expect("no link")
                .to_string();

            let null_line = Regex::new(r"^[\n\s]*$").unwrap();
            let mut text = String::new();
            for p in acv_comment.find(Name("p")) {
                let lines = p.children()
                    .filter(|node| if let &Data::Text(_) = node.data() {
                                true
                            } else {
                                false
                            })
                    .map(|text_node| text_node.as_text().unwrap());
                for line in lines {
                    if !null_line.is_match(line) {
                        text.push_str(line);
                    }
                }
                text.push('\n');
            }

            let images = x[1]
                .find(Class("view_img_link"))
                .map(|img| {
                         img.attr("href")
                             .expect("no href in view_img_link")
                             .to_string()
                     })
                .map(|src| if src.starts_with("//") {
                         let mut s = String::with_capacity(6 + src.len());
                         s.push_str("https:");
                         s.push_str(&src);
                         s
                     } else {
                         src
                     })
                .collect::<Vec<_>>();

            let vote = jandan_vote
                .find(Name("span"))
                .filter(|x| x.first_child().is_some())
                .map(|x| x.text())
                .collect::<Vec<_>>();

            let oo = vote.get(0).map_or(0, |s| s.parse::<u32>().unwrap_or(0));
            let xx = vote.get(1).map_or(0, |s| s.parse::<u32>().unwrap_or(0));

            let id = jandan_vote
                .find(Name("a"))
                .map(|a| a.attr("data-id"))
                .filter(|x| x.is_some())
                .map(|x| x.unwrap())
                .next()
                .unwrap()
                .to_string();

            let comments = try!(get_comments(&id));

            Ok(Pic {
                   author: author,
                   link: link,
                   id: id,
                   oo: oo,
                   xx: xx,
                   text: text,
                   images: images,
                   comments: comments,
               })
        })
        .collect::<Result<Vec<Pic>>>()
}
