extern crate jandan_pic_bot;
extern crate curl;
extern crate serde_json;
extern crate telegram_bot;

use jandan_pic_bot::*;
use jandan_pic_bot::errors::*;

use std::fs::File;

use curl::easy::Easy;

use telegram_bot::Api;
use telegram_bot::ParseMode;

fn channel_id_to_int(bot_token: &str, id: &str) -> Result<i64> {
    if !id.starts_with('@') {
        panic!("Channel ID must be Integer or \"@channelusername\"");
    }
    let url = format!("https://api.telegram.org/bot{}/getChat?chat_id={}",
                      bot_token,
                      id);
    let mut buf: Vec<u8> = Vec::new();

    let mut client = Easy::new();
    client.url(&url)?;
    {
        let mut transfer = client.transfer();
        transfer.write_function(|data| {
                buf.extend_from_slice(data);
                Ok(data.len())
            })?;
        transfer.perform()?;
    }

    let data = serde_json::from_slice::<serde_json::Value>(&buf)?;
    if !data.find("ok").and_then(|ok| ok.as_bool()).unwrap_or(false) {
        panic!(data
            .find("description")
            .and_then(|s| s.as_str().map(|s| s.to_string()))
            .ok_or("can not find description")?
        );
    }

    data.find("result")
        .and_then(|d| d.find("id"))
        .and_then(|id| id.as_i64())
        .ok_or_else(|| "can not find result".into())
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
    let api = Api::from_token(&token).unwrap();

    let channel_id = channel_id.parse::<i64>()
        .or_else(|_| channel_id_to_int(&token, &channel_id))
        .unwrap();

    let data = spider::get_list().unwrap();

    let old_pic = match File::open("old_pic.list") {
        Ok(file) => {
            let l: serde_json::Value = serde_json::from_reader(file).unwrap();
            l.as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()).collect()
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

    for pic in &data {
        if old_pic.contains(&pic.id) {
            continue;
        }

        for img in &pic.images {
            // this is a bug in telegram_bot, Result always Err
            // so ignore it
            let _ = api.send_message(channel_id, img.to_string(), None, None, None, None);
        }
        let mut msg = format!("*{}*: {}\n{}*OO*: {} *XX*: {}",
                              &pic.author.replace("*", ""),
                              &pic.link,
                              telegram_md_escape(&pic.text),
                              pic.oo,
                              pic.xx);
        for comment in &pic.comments {
            msg.push_str(&format!("\n*{}*: {}\n*❤️*: {}",
                                  &comment.author.replace("*", ""),
                                  telegram_md_escape(&comment.text),
                                  comment.likes));
        }

        let _ = api.send_message(channel_id,
                                 msg,
                                 Some(ParseMode::Markdown),
                                 Some(true),
                                 None,
                                 None);

    }
}
