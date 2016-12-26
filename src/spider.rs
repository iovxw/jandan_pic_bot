use std::time::Duration;

use regex::Regex;

use curl::easy::Easy;

use serde_json;

use select::document::Document;
use select::predicate::{Predicate, Attr, Class, Name};

use errors::*;

const JANDAN_HOME: &'static str = "https://jandan.net/";
const DUOSHUO_API: &'static str = "http://jandan.duoshuo.com/api/threads/listPosts.json";

lazy_static! {
    static ref IMG_FILTER: Regex = Regex::new(r#"<img\s*src="(?P<s>[^"]*)".*>"#).unwrap();
    static ref BR_FILTER: Regex = Regex::new(r#"<br ?/>\r?\n?"#).unwrap();
    static ref AUTHOR_FILTER: Regex = Regex::new(r"^[^\s@]+").unwrap();
    static ref NULL_LINE_FILTER: Regex = Regex::new(r"^[\n\s]*$").unwrap();
}

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
    let result = IMG_FILTER.replace_all(comment, " $s ");
    let result = BR_FILTER.replace_all(&result, "\n");

    result.replace("&quot;", "\"")
        .replace("&amp;", "*")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

pub fn get_comments(id: &str) -> Result<Vec<Comment>> {
    let url = format!("{}?thread_key={}", DUOSHUO_API, id);

    let mut buf = Vec::new();

    let mut client = Easy::new();
    client.url(&url)?;
    client.timeout(Duration::from_secs(10))?;
    client.follow_location(true)?;
    {
        let mut transfer = client.transfer();
        transfer.write_function(|data| {
                buf.extend_from_slice(data);
                Ok(data.len())
            })?;
        transfer.perform()?;
    }

    let data = serde_json::from_slice::<serde_json::Value>(&buf)?;
    let comment_data = data.find("parentPosts").ok_or("can not find parentPosts")?;

    let mut comments = data.find("response")
        .and_then(|r| r.as_array()).ok_or("can not find response or response is not array")?.iter()
        .filter_map(|comment_id| comment_id.as_str())
        .filter_map(|comment_id| comment_data.find(comment_id))
        .map(|comment| match (
            comment.find("author").and_then(|a| a.find("name")).and_then(|n| n.as_str()),
            comment.find("likes").and_then(|l| l.as_u64()),
            comment.find("message").and_then(|m| m.as_str())
        ) {
            (Some(author), Some(likes), Some(text)) => Comment {
                author: author.to_string(),
                likes: likes,
                text: escape_html(text)
            },
            err => panic!("response format error: (author, likes, message) => {:?}", err)
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
    client.url(JANDAN_HOME)?;
    client.timeout(Duration::from_secs(10))?;
    client.follow_location(true)?;
    // jandan.net's certificate is invalid (CN is *.jandan.net), ignore it
    client.ssl_verify_peer(false)?;
    {
        let mut transfer = client.transfer();
        transfer.write_function(|data| {
                buf.extend_from_slice(data);
                Ok(data.len())
            })?;
        transfer.perform()?;
    }

    let document = Document::from(String::from_utf8_lossy(&buf).into_owned().as_str());

    document.find(Attr("id", "list-pic")).next()
        .find(Class("acv_author").or(Class("acv_comment"))).iter()
        .collect::<Vec<_>>()
        .chunks(2)
        .map(|x| (x[0], x[1]))
        .map(|(x0, x1)| {
            let author = x0.children().first()
                .and_then(|a| AUTHOR_FILTER
                    .captures(&a.text())
                    .and_then(|a| a.at(0).map(|a| a.to_string()))
                )
                .ok_or("can not find author")?;

            let link = x0.find(Name("a")).next().first()
                .and_then(|l| l.attr("href").map(|l| l.to_string()))
                .ok_or("can not find link")?;

            let id = link.split("#").last()
                .ok_or("can not find id")?
                .to_string();

            let text = x1.find(Name("p")).iter()
                .map(|p| p.children().first().map(|n| n.text()).unwrap())
                .filter(|line| !NULL_LINE_FILTER.is_match(&line))
                .collect::<Vec<_>>()
                .join("\n");

            let images = x1.find(Name("img")).next().iter()
                .map(|img| img
                    .attr("org_src")
                    .or_else(|| img.attr("src"))
                    .map(|src| src.to_string())
                    .unwrap()
                )
                .map(|src| if src.starts_with("//") {
                    format!("https:{}", src)
                } else {
                    src
                })
                .collect::<Vec<_>>();

            let vote = x1.find(Class("vote")).first().ok_or("can not found vote")?
                .find(Name("span")).iter()
                .filter_map(|v| v.next())
                .map(|v| v.text())
                .collect::<Vec<_>>();

            let oo = vote.get(0).map_or(0, |s| s.parse::<u32>().unwrap_or(0));
            let xx = vote.get(1).map_or(0, |s| s.parse::<u32>().unwrap_or(0));
            let comments = get_comments(&id)?;

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
