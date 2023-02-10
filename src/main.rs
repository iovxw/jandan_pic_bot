#![feature(iter_intersperse)]

use std::borrow::Cow;
use std::fmt::Write;
use std::fs;
use std::io::Cursor;
use std::time::Duration;

use anyhow::anyhow;
use futures::prelude::*;
use log::error;
use tbot::types::{
    input_file::{Animation, Document, Photo},
    parameters::{ChatId, Text},
};
mod spider;
mod wayback_machine;

const HISTORY_SIZE: usize = 100;
const HISTORY_FILE: &str = "history.text";
const TG_IMAGE_SIZE_LIMIT: u32 = 1280;

struct Image {
    format: image::ImageFormat,
    name: String,
    width: u32,
    height: u32,
    data: Vec<u8>,
}

impl Image {
    fn is_gif(&self) -> bool {
        matches!(self.format, image::ImageFormat::Gif)
    }
}

async fn download_image(url: &str) -> anyhow::Result<Image> {
    let url = reqwest::Url::parse(url)?;
    let name = url
        .path_segments()
        .map(|s| s.last())
        .flatten()
        .unwrap_or_default()
        .into();
    let buf = spider::CLIENT
        .with(|client| client.get(url).header("referer", "https://jandan.net/"))
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let reader = image::io::Reader::new(Cursor::new(&buf))
        .with_guessed_format()
        .expect("io read error in Cursor<Vec>?");
    let format = reader.format().ok_or_else(|| {
        image::ImageError::Unsupported(image::error::ImageFormatHint::Unknown.into())
    })?;
    let dimensions = reader.into_dimensions()?;
    Ok(Image {
        format,
        name,
        width: dimensions.0,
        height: dimensions.1,
        data: buf.to_vec(),
    })
}

// TODO: CoW
fn telegram_md_escape(s: &str) -> String {
    s.replace("[", "\\[")
        .replace("*", "\\*")
        .replace("_", "\\_")
        .replace("`", "\\`")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    let token = std::env::args()
        .nth(1)
        .ok_or(anyhow!("Need a Telegram bot token as argument"))?;
    let channel_id = std::env::args()
        .nth(2)
        .ok_or(anyhow!("Please specify a Telegram Channel"))?;
    let wayback_machine_token = std::env::args().nth(3);

    let bot = tbot::Bot::new(token);
    let channel: ChatId = channel_id.as_str().into();
    let history = fs::read_to_string(HISTORY_FILE)?;
    let history: Vec<&str> = history.lines().collect();
    let pics = spider::do_the_evil().await?;
    let mut fresh_imgs: Vec<Cow<str>> = Vec::with_capacity(HISTORY_SIZE);

    for pic in pics.into_iter().filter(|pic| !history.contains(&&*pic.id)) {
        send_pic(&bot, channel, &pic).await?;

        fresh_imgs.push(pic.id.into());
    }

    fs::write(
        HISTORY_FILE,
        fresh_imgs
            .iter()
            .map(|s| &**s)
            .chain(history.into_iter())
            .take(HISTORY_SIZE)
            .intersperse("\n".into())
            .collect::<String>(),
    )?;

    if let Some(token) = wayback_machine_token {
        wayback_machine::push(&token, &fresh_imgs).await?;
    }
    Ok(())
}

async fn send_pic(bot: &tbot::Bot, target: ChatId<'_>, pic: &spider::Pic) -> anyhow::Result<()> {
    let images: Vec<_> = futures::stream::iter(&pic.images)
        .then(|url| async move {
            match download_image(url).await {
                Ok(r) => Ok(r),
                Err(e) => Err((e, url)),
            }
        })
        .collect()
        .await;
    for img_result in &images {
        match img_result {
            Ok(img) => {
                if img.is_gif() {
                    bot.send_animation(target, Animation::with_bytes(&img.data))
                        .is_notification_disabled(true)
                        .call()
                        .await?;
                } else {
                    if std::cmp::max(img.width, img.height) > TG_IMAGE_SIZE_LIMIT {
                        bot.send_document(target, Document::with_bytes(&img.name, &img.data))
                            .is_notification_disabled(true)
                            .call()
                            .await?;
                    } else {
                        let p = Photo::with_bytes(&img.data);

                        bot.send_photo(target, p)
                            .is_notification_disabled(true)
                            .call()
                            .await?;
                    };
                }
            }
            Err((e, img_url)) => {
                error!("{}: {}", img_url, e);
                bot.send_message(target, *img_url)
                    .is_notification_disabled(true)
                    .call()
                    .await?;
            }
        }

        tokio::time::delay_for(Duration::from_secs(3)).await;
    }

    let caption = format_caption(pic);
    bot.send_message(target, Text::with_markdown(&caption))
        .is_web_page_preview_disabled(true)
        .call()
        .await?;
    Ok(())
}

fn format_caption(pic: &spider::Pic) -> String {
    let mut msg = format!(
        "*{}*: https://jandan.net/t/{}\n",
        pic.author.replace("*", ""),
        pic.id,
    );
    if !pic.text.is_empty() {
        msg.push_str(&telegram_md_escape(&pic.text));
        msg.push('\n');
    }
    write!(msg, "*OO*: {} *XX*: {}", pic.oo, pic.xx).unwrap();
    for comment in &pic.comments {
        write!(
            msg,
            "\n*{}*: {}\n*OO*: {}, *XX*: {}",
            &comment.author.replace("*", ""),
            telegram_md_escape(&comment.content),
            comment.oo,
            comment.xx
        )
        .unwrap();
    }
    msg
}
