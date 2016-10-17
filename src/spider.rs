use std::io;

use regex::Regex;

use hyper::client::Client;

use serde_json;

use select::document::Document;
use select::node::Data;
use select::predicate::{Predicate, Attr, Class, Name};

const JANDAN_HOME: &'static str = "http://jandan.net/";
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

// TODO: fix Result
pub fn get_comments(id: &str) -> Vec<Comment> {
    let url = format!("{}?thread_key={}", DUOSHUO_API, id);

    let client = Client::new();
    let res = client.get(&url).send().unwrap();
    if !res.status.is_success() {
        panic!(res.status);
    }

    let data: serde_json::Value = serde_json::from_reader(res).unwrap();
    let all_comments = data.find("parentPosts").unwrap();

    data.find("hotPosts").unwrap().as_array().unwrap().iter()
        .map(|id| all_comments.find(id.as_str().unwrap()))
        .filter(|result| result.is_some())
        .map(|result| result.unwrap())
        .map(|comment| {
            let author_info = comment.find("author").unwrap();
            Comment {
                author: author_info.find("name")
                    .expect("undefined \"name\" in comment").as_str().unwrap().to_string(),
                likes: comment.find("likes")
                    .expect("undefined \"likes\" in comment").as_u64().unwrap(),
                text: comment.find("message")
                    .expect("undefined \"message\" in comment").as_str().unwrap().to_string()
        }})
        .collect::<Vec<_>>()
}

// TODO: fix Result
pub fn get_list<'a>() -> io::Result<Vec<Pic>> {
    let client = Client::new();

    let mut res = client.get(JANDAN_HOME).send().unwrap();
    if !res.status.is_success() {
        panic!(res.status);
    }

    let document = Document::from_read(&mut res).unwrap();

    let re = Regex::new(r"^[^\s@]+").unwrap();
    //let mut result: Vec<Pic> = Vec::new();
    let result = document.find(Attr("id", "list-pic"))
        .next()
        .unwrap()
        .find(Class("acv_author").or(Class("acv_comment")))
        .collect::<Vec<_>>()
        .chunks(2)
        .map (|x| {
            let author_raw = x[0].first_child().unwrap().text();
            let author = re.captures(&author_raw).unwrap().at(0).unwrap().to_string();
            let link = x[0].find(Name("a")).next().unwrap().attr("href").unwrap().to_string();

            let id = link.split('#').last().unwrap().to_string();

            let null_line = Regex::new(r"^[\n\s]*$").unwrap();
            let text = x[1].find(Name("p")).next().unwrap().children()
                .filter(|node| if let &Data::Text(_) = node.data() { true } else { false })
                .map(|text_node| text_node.as_text().unwrap())
                .fold(String::new() ,
                      |text1, text2| if null_line.is_match(text2) { text1 } else { text1 + text2 });

            let images = x[1].find(Name("img"))
                .map(|img| img.attr("src").unwrap().to_string())
                .collect::<Vec<_>>();

            let vote = x[1].find(Class("vote")).next().unwrap()
                .find(Name("span"))
                .filter(|x| x.first_child().is_some())
                .map(|x| x.text()).collect::<Vec<_>>();

            let oo = if let Some(oo) = vote.get(0) { oo.parse::<u32>().unwrap_or(0) } else { 0 };
            let xx = if let Some(xx) = vote.get(1) { xx.parse::<u32>().unwrap_or(0) } else { 0 };

            let comments = get_comments(&id);

            Pic {
                author: author,
                link: link,
                id: id,
                oo: oo,
                xx: xx,
                text: text,
                images: images,
                comments: comments,
            }
        })
        .collect();

    Ok(result)
}
