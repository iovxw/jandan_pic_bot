#![feature(conservative_impl_trait)]

extern crate curl;
extern crate tokio_curl;
extern crate futures;
extern crate tokio_core;
extern crate telebot;
#[macro_use]
extern crate error_chain;
extern crate regex;
extern crate kuchiki;
#[macro_use]
extern crate serde_derive;
extern crate serde;
extern crate serde_json;
#[macro_use]
extern crate lazy_static;

mod errors;
mod spider;

use std::fs::File;
use std::time::Duration;

use curl::easy::Easy;
use futures::{Future, Stream};
use tokio_curl::Session;
use tokio_core::reactor::Core;
use telebot::bot;
use telebot::functions::*;

fn channel_id_to_int(bot_token: &str, id: &str) -> i64 {
    if !id.starts_with('@') {
        panic!("Channel ID must be Integer or \"@channelusername\"");
    }
    let url = format!("https://api.telegram.org/bot{}/getChat?chat_id={}",
                      bot_token,
                      id);
    let mut buf: Vec<u8> = Vec::new();

    let mut client = Easy::new();
    client.url(&url).unwrap();
    client.timeout(Duration::from_secs(10)).unwrap();
    {
        let mut transfer = client.transfer();
        transfer
            .write_function(|data| {
                                buf.extend_from_slice(data);
                                Ok(data.len())
                            })
            .unwrap();
        transfer.perform().unwrap();
    }

    let data: serde_json::Value = serde_json::from_slice(&buf).unwrap();
    if !data.get("ok").unwrap().as_bool().unwrap() {
        panic!(data.get("description")
                   .unwrap()
                   .as_str()
                   .unwrap()
                   .to_string());
    }

    data.get("result")
        .unwrap()
        .get("id")
        .unwrap()
        .as_i64()
        .unwrap()
}

fn telegram_md_escape(s: &str) -> String {
    s.replace("[", "\\[")
        .replace("*", "\\*")
        .replace("_", "\\_")
        .replace("`", "\\`")
}

fn main() {
    let token = std::env::args()
        .nth(1)
        .expect("Need a Telegram bot token as argument");
    let channel_id = std::env::args()
        .nth(2)
        .expect("Please specify a Telegram Channel");

    let mut lp = Core::new().unwrap();

    let bot = bot::RcBot::new(lp.handle(), &token);

    let channel_id = channel_id
        .parse::<i64>()
        .unwrap_or_else(|_| channel_id_to_int(&token, &channel_id));

    let session = Session::new(lp.handle());

    let data = spider::get_list(session);

    let old_pic = match File::open("old_pic.list") {
        Ok(file) => {
            let l: serde_json::Value = serde_json::from_reader(file).unwrap();
            l.as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_str().unwrap().to_string())
                .collect()
        }
        Err(_) => Vec::new(),
    };

    let r = data.filter(|pic| !old_pic.contains(&pic.id))
        .and_then(|pic| {
            let spider::Pic {
                author,
                link,
                id,
                oo,
                xx,
                text,
                images,
                comments,
            } = pic;
            let imgs = futures::stream::iter(images.into_iter().map(Ok))
                .and_then(|img| bot.message(channel_id, img).send());

            let mut msg = format!("*{}*: {}\n{}*OO*: {} *XX*: {}",
                                  &author.replace("*", ""),
                                  &link,
                                  telegram_md_escape(&text),
                                  oo,
                                  xx);
            for comment in &comments {
                msg.push_str(&format!("\n*{}*: {}\n*OO*: {}, *XX*: {}",
                                     &comment.author.replace("*", ""),
                                     telegram_md_escape(&comment.content),
                                     comment.oo,
                                     comment.xx));
            }

            imgs.for_each(|_| Ok(()))
                .and_then(|_| {
                              bot.message(channel_id, msg)
                                  .parse_mode("Markdown")
                                  .disable_web_page_preview(true)
                                  .send()
                          })
                .map_err(|e| format!("Telegram: {:?}", e).into())
                .map(move |_| id)
        })
        .collect();
    let new_pic = lp.run(r).unwrap();
    let mut file = File::create("old_pic.list").unwrap();
    let id_list = new_pic
        .iter()
        .chain(old_pic.iter())
        .take(100)
        .collect::<Vec<&String>>();
    serde_json::to_writer(&mut file, &id_list).unwrap();
}
