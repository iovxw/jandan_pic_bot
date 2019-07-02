#![feature(proc_macro, generators, crate_in_paths)]

extern crate curl;
extern crate futures_await as futures;
extern crate telebot;
extern crate tokio_core;
extern crate tokio_curl;
#[macro_use]
extern crate failure;
extern crate kuchiki;
extern crate regex;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;
#[macro_use]
extern crate lazy_static;
extern crate base64;
extern crate env_logger;
extern crate image;
extern crate log;

mod spider;

use std::fs::File;
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use curl::easy::Easy;
use failure::{err_msg, ResultExt};
use futures::prelude::*;
use image::GenericImage;
use telebot::bot;
use telebot::file::MediaFile;
use telebot::functions::*;
use tokio_core::reactor::Core;
use tokio_curl::Session;

const TG_IMAGE_SIZE_LIMIT: u32 = 1280;

type Result<T> = std::result::Result<T, failure::Error>;

fn channel_id_to_int(bot_token: &str, id: &str) -> i64 {
    if !id.starts_with('@') {
        panic!("Channel ID must be Integer or \"@channelusername\"");
    }
    let url = format!(
        "https://api.telegram.org/bot{}/getChat?chat_id={}",
        bot_token, id
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
) -> impl Future<Item = Vec<u8>, Error = failure::Error> {
    let buf = Arc::new(Mutex::new(Vec::new()));

    let mut req = Easy::new();
    req.url(url).unwrap();
    req.timeout(Duration::from_secs(60)).unwrap();
    req.follow_location(true).unwrap();
    {
        let buf = Arc::clone(&buf);
        req.write_function(move |data| {
            buf.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }).unwrap();
    }
    session
        .perform(req)
        .map_err(|e| format_err!("failed to download image: {}", e))
        .and_then(|mut resp| {
            let code = resp.response_code().unwrap();
            if code != 200 {
                return Err(format_err!("failed to download image: HTTP {}", code));
            }
            std::mem::drop(resp);
            Ok(Arc::try_unwrap(buf).unwrap().into_inner().unwrap())
        })
}

// FIXME: futures-await
fn video_send_failed(
    r: Result<(bot::RcBot, telebot::objects::Message)>,
) -> impl Future<Item = bool, Error = failure::Error> {
    match r {
        Ok((bot, msg)) => {
            let chat_id = msg.chat.id;
            let msg_id = msg.message_id;
            let failed = (|| -> Option<bool> {
                let video = msg.video.as_ref()?;
                Some(video.mime_type.as_ref()? != "video/mp4" || video.duration == 0)
            })().unwrap_or(true);
            let failed2 = (move || -> Option<bool> {
                let video = msg.document?;
                Some(video.mime_type? != "video/mp4")
            })().unwrap_or(true);
            if failed && failed2 {
                futures::future::Either::A(bot.delete_message(chat_id, msg_id).send().map(|_| true))
            } else {
                futures::future::Either::B(futures::future::ok(false))
            }
        }
        Err(e) => {
            futures::future::Either::B(futures::future::ok(true))
        }
    }
}

#[async]
fn send_image_to(
    bot: bot::RcBot,
    channel_id: i64,
    session: Session,
    images: Vec<String>,
) -> Result<()> {
    for img_link in images {
        let send_link = bot
            .message(channel_id, img_link.clone())
            .disable_notification(true);
        let data = await!(download_file(&session, &img_link));
        if data.is_err() {
            eprintln!("Failed to download image: {}", img_link);
            await!(send_link.send())?;
            continue;
        }
        let data = data.unwrap();
        let is_gif = img_link.ends_with(".gif");
        let img = if is_gif {
            image::load_from_memory_with_format(&data, image::ImageFormat::GIF)
        } else {
            image::load_from_memory(&data)
        };
        if img.is_err() {
            eprintln!("Failed to parse image: {}", img_link);
            await!(send_link.send())?;
            continue;
        }
        let img = img.unwrap();
        if std::cmp::max(img.width(), img.height()) > TG_IMAGE_SIZE_LIMIT {
            println!("Image is too large: {}", img_link);
            await!(send_link.send())?;
        } else if is_gif {
            let send_by_link = await!(
                bot.animation(channel_id)
                    .animation(MediaFile::SingleFile(img_link.clone()))
                    .disable_notification(true)
                    .send()
            );
            if await!(video_send_failed(send_by_link))? {
                eprintln!("Failed to send video by link: {}", img_link);
                let send_by_file = await!(
                    bot.animation(channel_id)
                        .file((img_link.as_str(), Cursor::new(data)))
                        .disable_notification(true)
                        .send()
                );
                if await!(video_send_failed(send_by_file))? {
                    eprintln!("Failed to send video by file: {}", img_link);
                    await!(send_link.send())?;
                }
            }
        } else {
            let send_by_link = await!(
                bot.photo(channel_id)
                    .file(telebot::File::Url(img_link.clone()))
                    .disable_notification(true)
                    .send()
            );
            if send_by_link.is_err() {
                if let Err(e) = send_by_link {
                    eprintln!("Failed to send photo by link: {}\n{:?}", img_link, e);
                }
                let read = Cursor::new(data);
                let send_by_file = await!(
                    bot.photo(channel_id)
                        .file((img_link.as_str(), read))
                        .disable_notification(true)
                        .send()
                );
                if send_by_file.is_err() {
                    if let Err(e) = send_by_file {
                        eprintln!("Failed to send photo by file: {}\n{:?}", img_link, e);
                    }
                    await!(send_link.send())?;
                }
            }
        }
    }
    Ok(())
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "UPPERCASE")]
struct LivereResp {
    best_re_list: Option<Vec<LivereComment>>,
}

#[derive(Deserialize, Debug)]
struct LivereComment {
    bad: u32,
    good: u32,
    content: String,
    name: String,
}

pub fn get_livere_comments<'a>(
    session: &Session,
    id: &str,
) -> impl Future<Item = Vec<spider::Comment>, Error = failure::Error> + 'a {
    let url = format!("https://api-city.livere.com/livereDataLoad?refer=jandan.net/yellowcomment-{}\
                       &version=lv9\
                       &consumer_seq=1020\
                       &livere_seq=45041", id);

    let (request, body) = spider::make_request(&url).unwrap();

    let req = session.perform(request).map_err(|e| format_err!("{}", e));

    req.and_then(move |mut resp| {
        assert_eq!(resp.response_code().unwrap(), 200);
        let body = body.lock().unwrap();
        let resp = serde_json::from_slice::<LivereResp>(&body)?;
        if resp.best_re_list.is_none() {
            return Ok(Vec::new());
        }
        resp.best_re_list
            .unwrap()
            .into_iter()
            .map(|comment| {
                Ok(spider::Comment {
                    author: comment.name,
                    oo: comment.good,
                    xx: comment.bad,
                    content: comment.content,
                })
            })
            .collect::<Result<_>>()
    })
}

// FIXME: Everything is just work
fn main() -> Result<()> {
    env_logger::init();
    let token = std::env::args()
        .nth(1)
        .ok_or(err_msg("Need a Telegram bot token as argument"))?;
    let channel_id = std::env::args()
        .nth(2)
        .ok_or(err_msg("Please specify a Telegram Channel"))?;

    let mut lp = Core::new().unwrap();

    let bot = bot::RcBot::new(lp.handle(), &token);

    let channel_id = channel_id
        .parse::<i64>()
        .unwrap_or_else(|_| channel_id_to_int(&token, &channel_id));

    let session = Session::new(lp.handle());

    let data = spider::get_list(session);

    let old_pic = File::open("old_pic.list")
        .context("failed to open old_pic.list")
        .and_then(|file| {
            serde_json::from_reader::<_, Vec<String>>(file)
                .context("illegal data format in old_pic.list")
        })
        .unwrap_or_default();

    let handle = lp.handle();
    let r = data
        .filter(|pic| !old_pic.contains(&pic.id))
        .and_then(move |pic| {
            let handle = handle.clone();
            let bot = bot.clone();
            let spider::Pic {
                author,
                link: _link,
                id,
                oo,
                xx,
                text,
                images,
                comments,
            } = pic;
            let bot2 = bot.clone();
            let bot3 = bot.clone();
            let session = Session::new(handle.clone());
            let imgs = send_image_to(bot.clone(), channel_id, session, images);

            let mut msg = format!(
                "*{}*: https://jandan.net/t/{}\n{}*OO*: {} *XX*: {}",
                &author.replace("*", ""),
                &id,
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

            let session = Session::new(handle.clone());
            let get_livere_comments_future = get_livere_comments(&session, &id);

            imgs.and_then(move |_| {
                bot2.message(channel_id, msg)
                    .parse_mode(ParseMode::Markdown)
                    .disable_web_page_preview(true)
                    .send()
            }).and_then(move |_| {
                get_livere_comments_future
            }).and_then(move |comments| {
                if comments.is_empty() {
                    return futures::future::Either::A(futures::future::ok(()));
                }
                let mut msg = "以下来自 ColtIsGayGay 的第三方吐槽，[Chrome 插件](http://jandan.net/t/4289693)".to_string();
                for comment in &comments {
                    msg.push_str(&format!(
                        "\n*{}*: {}\n*OO*: {}, *XX*: {}",
                        &comment.author.replace("*", ""),
                        telegram_md_escape(&comment.content),
                        comment.oo,
                        comment.xx
                    ));
                }
                let f = bot3.message(channel_id, msg)
                    .parse_mode(ParseMode::Markdown)
                    .disable_web_page_preview(true)
                    .send()
                    .map(|_| ());
                futures::future::Either::B(f)
            }).map(move |_| id)
        })
        .collect();
    let new_pic = lp.run(r)?;
    let mut file = File::create("old_pic.list").context("failed to create old_pic.list")?;
    let id_list = new_pic
        .iter()
        .chain(old_pic.iter())
        .take(100)
        .collect::<Vec<&String>>();
    serde_json::to_writer(&mut file, &id_list).context("failed to save data to old_pic.list")?;
    Ok(())
}
