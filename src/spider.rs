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
const DUOSHUO_API: &'static str = "http://jandan.duoshuo.com/api/threads/listPosts.json";

#[derive(Clone, Debug, PartialEq)]
pub struct Comment {
    pub author: String,
    pub likes: u64,
    pub text: String,
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

fn escape_html(comment: &str) -> String {
    let img = Regex::new(r#"<img\s*src="(?P<s>[^"]*)".*>"#).unwrap();
    let br = Regex::new(r#"<br ?/>\r?\n?"#).unwrap();

    let result = img.replace_all(comment, " $s ");
    let result = br.replace_all(&result, "\n");

    result.replace("&quot;", "\"")
        .replace("&amp;", "*")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

pub fn get_comments(id: &str) -> Result<Vec<Comment>> {
    let url = format!("{}?thread_key={}", DUOSHUO_API, id);

    let mut buf: Vec<u8> = Vec::new();

    let mut client = Easy::new();
    client.url(&url).unwrap();
    client.timeout(Duration::from_secs(10)).unwrap();
    client.follow_location(true).unwrap();
    {
        let mut transfer = client.transfer();
        transfer.write_function(|data| {
                buf.extend_from_slice(data);
                Ok(data.len())
            })
            .unwrap();
        try!(transfer.perform());
    }

    let data: serde_json::Value = try!(serde_json::from_slice(&buf));
    let comment_data = data.find("parentPosts")
        .expect("can not find parentPosts");

    let mut comments = data.find("response")
        .expect("can not find response")
        .as_array()
        .expect("response is not array")
        .iter()
        .map(|comment_id| {
            let comment_id = comment_id.as_str().unwrap();
            let comment = comment_data.find(comment_id).unwrap();

            let author_info = comment.find("author").unwrap();
            let author = author_info.find("name")
                .expect("undefined \"name\" in comment")
                .as_str()
                .unwrap()
                .to_string();
            let likes = comment.find("likes")
                .expect("undefined \"likes\" in comment")
                .as_u64()
                .unwrap();
            let text = escape_html(comment.find("message")
                .expect("undefined \"message\" in comment")
                .as_str()
                .unwrap());
            Comment {
                author: author,
                likes: likes,
                text: text,
            }
        })
        .collect::<Vec<Comment>>();

    if comments.len() > 3 {
        comments.sort_by(|c1, c2| c2.likes.cmp(&c1.likes));
        comments.truncate(3);
    }

    Ok(comments)
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
        transfer.write_function(|data| {
                buf.extend_from_slice(data);
                Ok(data.len())
            })
            .unwrap();
        try!(transfer.perform());
    }

    let buf = io::Cursor::new(buf);

    let document = try!(Document::from_read(buf));

    document.find(Attr("id", "list-pic"))
        .next()
        .expect("can not find #list-pic")
        .find(Class("acv_author").or(Class("acv_comment")))
        .collect::<Vec<_>>()
        .chunks(2)
        .map(|x| {
            let author_raw = x[0].first_child().unwrap().text();
            let author_filter = Regex::new(r"^[^\s@]+").unwrap();
            let author = author_filter.captures(&author_raw).unwrap().at(0).unwrap().to_string();

            let link = x[0].find(Name("a")).next().unwrap().attr("href").unwrap().to_string();

            let id = link.split('#').last().unwrap().to_string();

            let null_line = Regex::new(r"^[\n\s]*$").unwrap();
            let mut text = String::new();
            for p in x[1].find(Name("p")) {
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

            let images = x[1].find(Name("img"))
                .map(|img| img.attr("org_src").unwrap_or(img.attr("src").unwrap()).to_string())
                .map(|src| if src.starts_with("//") {
                    let mut s = String::with_capacity(6 + src.len());
                    s.push_str("https:");
                    s.push_str(&src);
                    s
                } else {
                    src
                })
                .collect::<Vec<_>>();

            let vote = x[1].find(Class("vote"))
                .next()
                .unwrap()
                .find(Name("span"))
                .filter(|x| x.first_child().is_some())
                .map(|x| x.text())
                .collect::<Vec<_>>();

            let oo = vote.get(0).map_or(0, |s| s.parse::<u32>().unwrap_or(0));
            let xx = vote.get(1).map_or(0, |s| s.parse::<u32>().unwrap_or(0));

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
