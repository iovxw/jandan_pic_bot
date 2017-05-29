extern crate jandan_pic_bot;
extern crate curl;
extern crate serde_json;
extern crate futures;
extern crate tokio_core;
extern crate telebot;

use jandan_pic_bot::*;

use std::fs::File;
use std::time::Duration;

use curl::easy::Easy;
use futures::*;
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

    let data = spider::get_list().unwrap();

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

    let mut pic_id_list = data.iter()
        .map(|p| &p.id)
        .chain(old_pic.iter())
        .collect::<Vec<_>>();
    pic_id_list.sort(); // sort for more efficient deduplication
    pic_id_list.dedup();
    if pic_id_list.len() > 100 {
        pic_id_list.reverse();
        pic_id_list.truncate(100);
        pic_id_list.reverse();
    }

    let mut file = File::create("old_pic.list").unwrap();
    serde_json::to_writer(&mut file, &pic_id_list).unwrap();

    let mut msgs = Vec::new();

    for pic in &data {
        if old_pic.contains(&pic.id) {
            continue;
        }

        for img in &pic.images {
            msgs.push(bot.message(channel_id, img.to_owned()));
        }
        let mut msg = format!("*{}*: {}\n{}*OO*: {} *XX*: {}",
                              &pic.author.replace("*", ""),
                              &pic.link,
                              telegram_md_escape(&pic.text),
                              pic.oo,
                              pic.xx);
        for comment in &pic.comments {
            msg.push_str(&format!("\n*{}*: {}\n*OO*: {}, *XX*: {}",
                                 &comment.author.replace("*", ""),
                                 telegram_md_escape(&comment.content),
                                 comment.oo,
                                 comment.xx));
        }

        msgs.push(bot.message(channel_id, msg)
                      .parse_mode("Markdown")
                      .disable_web_page_preview(true));
    }

    let mut future = futures::future::ok(()).boxed() as
                     Box<Future<Item = (), Error = telebot::Error>>;
    for msg in msgs {
        future = Box::new(future.and_then(|_| msg.send().and_then(|_| Ok(())))) as
                 Box<Future<Item = (), Error = telebot::Error>>
    }
    lp.run(future).unwrap();
}
