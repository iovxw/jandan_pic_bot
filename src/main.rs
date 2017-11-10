#![feature(proc_macro, conservative_impl_trait, generators)]

extern crate curl;
extern crate futures_await as futures;
extern crate tokio_curl;
extern crate tokio_core;
extern crate telebot;
#[macro_use]
extern crate error_chain;
extern crate regex;
extern crate kuchiki;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;
#[macro_use]
extern crate lazy_static;
extern crate image;

mod errors;
mod spider;

use errors::*;

use std::fs::File;
use std::time::Duration;
use std::sync::{Arc, Mutex};
use std::io::Cursor;

use curl::easy::Easy;
use futures::prelude::*;
use tokio_curl::Session;
use tokio_core::reactor::Core;
use telebot::bot;
use telebot::functions::*;
use image::GenericImage;

const TG_IMAGE_SIZE_LIMIT: u32 = 1280;

fn channel_id_to_int(bot_token: &str, id: &str) -> i64 {
    if !id.starts_with('@') {
        panic!("Channel ID must be Integer or \"@channelusername\"");
    }
    let url = format!(
        "https://api.telegram.org/bot{}/getChat?chat_id={}",
        bot_token,
        id
    );
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
        panic!(
            data.get("description")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        );
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

// TODO: code reuse
fn download_file(
    session: &Session,
    url: &str,
) -> impl Future<Item = Vec<u8>, Error = tokio_curl::PerformError> {
    let buf = Arc::new(Mutex::new(Vec::new()));

    let mut req = Easy::new();
    req.url(url).unwrap();
    req.timeout(Duration::from_secs(10)).unwrap();
    req.follow_location(true).unwrap();
    {
        let buf = Arc::clone(&buf);
        req.write_function(move |data| {
            buf.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }).unwrap();
    }
    session.perform(req).map(|mut resp| {
        assert_eq!(resp.response_code().unwrap(), 200);
        std::mem::drop(resp);
        Arc::try_unwrap(buf).unwrap().into_inner().unwrap()
    })
}

// A dirty fix
#[allow(unknown_lints, needless_pass_by_value)]
fn fix_telebot_err(e: telebot::Error) -> Error {
    format!("Telegram: {:?}", e).into()
}

#[async]
fn send_image_to(
    bot: bot::RcBot,
    channel_id: i64,
    session: Session,
    images: Vec<String>,
) -> Result<()> {
    for img_link in images {
        let data = await!(download_file(&session, &img_link)).chain_err(
            || "failed to download image",
        )?;
        let img_type = image::guess_format(&data).chain_err(
            || "unknown image format",
        )?;
        let img = image::load_from_memory_with_format(&data, img_type)
            .chain_err(|| "failed to decode image")?;
        if std::cmp::max(img.width(), img.height()) > TG_IMAGE_SIZE_LIMIT {
            await!(bot.message(channel_id, img_link).send()).map_err(
                fix_telebot_err,
            )?;
        } else if let image::GIF = img_type {
            let send_by_link = await!(bot.document(channel_id).url(img_link.as_str()).send());
            if send_by_link.is_err() {
                let read = Cursor::new(data);
                let send_by_file = bot.document(channel_id)
                    .file((img_link.as_str(), read))
                    .send();
                await!(send_by_file).map_err(fix_telebot_err)?;
            }
        } else {
            let send_by_link = await!(bot.photo(channel_id).url(img_link.as_str()).send());
            if send_by_link.is_err() {
                let read = Cursor::new(data);
                let send_by_file = bot.photo(channel_id).file((img_link.as_str(), read)).send();
                await!(send_by_file).map_err(fix_telebot_err)?;
            }
        }
    }
    Ok(())
}

// FIXME: Everything is just work
fn run() -> Result<()> {
    let token = std::env::args().nth(1).ok_or(
        "Need a Telegram bot token as argument",
    )?;
    let channel_id = std::env::args().nth(2).ok_or(
        "Please specify a Telegram Channel",
    )?;

    let mut lp = Core::new().unwrap();

    let bot = bot::RcBot::new(lp.handle(), &token);

    let channel_id = channel_id.parse::<i64>().unwrap_or_else(|_| {
        channel_id_to_int(&token, &channel_id)
    });

    let session = Session::new(lp.handle());

    let data = spider::get_list(session);

    let old_pic = File::open("old_pic.list")
        .chain_err(|| "failed to open old_pic.list")
        .and_then(|file| {
            serde_json::from_reader::<_, Vec<String>>(file).chain_err(
                || "illegal data format in old_pic.list",
            )
        })
        .unwrap_or_default();

    let handle = lp.handle();
    let r = data.filter(|pic| !old_pic.contains(&pic.id))
        .and_then(move |pic| {
            let handle = handle.clone();
            let bot = bot.clone();
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
            let bot2 = bot.clone();
            let session = Session::new(handle.clone());
            let imgs = send_image_to(bot.clone(), channel_id, session, images);

            let mut msg = format!(
                "*{}*: {}\n{}*OO*: {} *XX*: {}",
                &author.replace("*", ""),
                &link,
                telegram_md_escape(&text),
                oo,
                xx
            );
            for comment in &comments {
                msg.push_str(&format!(
                    "\n*{}*: {}\n*OO*: {}, *XX*: {}",
                    &comment.author.replace("*", ""),
                    telegram_md_escape(&comment.content),
                    comment.oo,
                    comment.xx
                ));
            }

            imgs.and_then(move |_| {
                bot2.message(channel_id, msg)
                    .parse_mode("Markdown")
                    .disable_web_page_preview(true)
                    .send()
                    .map_err(fix_telebot_err)
            }).map(move |_| id)
        })
        .collect();
    let new_pic = lp.run(r)?;
    let mut file = File::create("old_pic.list").chain_err(
        || "failed to create old_pic.list",
    )?;
    let id_list = new_pic
        .iter()
        .chain(old_pic.iter())
        .take(100)
        .collect::<Vec<&String>>();
    serde_json::to_writer(&mut file, &id_list).chain_err(|| "failed to save data to old_pic.list")
}

quick_main!(run);
