use std::io;

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
    let all_comments = data.find("parentPosts").unwrap();

    data.find("hotPosts")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|id| all_comments.find(id.as_str().unwrap()))
        .filter(|result| result.is_some())
        .map(|result| result.unwrap())
        .map(|comment| {
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
            Ok(Comment {
                author: author,
                likes: likes,
                text: text,
            })
        })
        .collect::<Result<Vec<Comment>>>()
}

pub fn get_list() -> Result<Vec<Pic>> {
    let mut buf: Vec<u8> = Vec::new();

    let mut client = Easy::new();
    client.url(JANDAN_HOME).unwrap();
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
        .unwrap()
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
            let text = x[1].find(Name("p"))
                .next()
                .unwrap()
                .children()
                .filter(|node| if let &Data::Text(_) = node.data() {
                    true
                } else {
                    false
                })
                .map(|text_node| text_node.as_text().unwrap())
                .fold(String::new(), |text1, text2| if null_line.is_match(text2) {
                    text1
                } else {
                    text1 + text2
                });

            let images = x[1].find(Name("img"))
                .map(|img| img.attr("org_src").unwrap_or(img.attr("src").unwrap()).to_string())
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
