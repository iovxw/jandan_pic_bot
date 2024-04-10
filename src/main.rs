#![feature(iter_intersperse)]

use std::borrow::Cow;
use std::fmt::Write;
use std::fs;
use std::io::Cursor;
use std::time::Duration;

use anyhow::anyhow;
use backon::{ConstantBuilder, Retryable};
use convert::video_to_mp4;
use futures::prelude::*;
use log::error;
use tbot::types::{
    input_file::{Document, GroupMedia, Photo, Video},
    parameters::{ChatId, Text},
};

mod convert;
mod spider;
mod wayback_machine;

const HISTORY_SIZE: usize = 100;
const HISTORY_FILE: &str = "history.text";
const TG_IMAGE_DIMENSION_LIMIT: u32 = 1280;
const LOW_QUALITY_IMG_SIZE: usize = 200 * 1024;

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
    let images: Vec<Result<Image, (_, &str)>> = futures::stream::iter(&pic.images)
        .then(|url| async move {
            match (|| async { download_image(url).await })
                .retry(
                    &ConstantBuilder::default()
                        .with_delay(Duration::from_secs(1))
                        .with_max_times(3),
                )
                .when(|e| true) // TODO: only when timeout
                .await
            {
                Ok(r) => Ok(r),
                Err(e) => Err((e, url.as_str())),
            }
        })
        .collect()
        .await;

    let caption = format_caption(pic);
    let caption = Text::with_markdown(&caption);
    let contains_error = images.iter().any(|r| r.is_err());
    let contains_large_image = images
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .any(|img| image_too_large(img));
    let contains_gif = images
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .any(|img| img.is_gif());
    if images.is_empty() || contains_error || contains_large_image && contains_gif {
        send_the_old_way(bot, target, images, caption).await?;
        return Ok(());
    }
    assert!(!images.is_empty());
    if contains_large_image {
        assert!(!contains_gif);
        // TODO: replace with:
        // send_as_document_group(bot, target, images, caption).await?;
        if images.len() == 1 {
            let img: Image = images.into_iter().find_map(|x| x.ok()).unwrap();
            let doc = Document::with_bytes(&img.name, &img.data).caption(caption);
            bot.send_document(target, doc)
                .is_notification_disabled(true)
                .call()
                .await?;
        } else {
            send_the_old_way(bot, target, images, caption).await?;
        }
    } else {
        let images: Vec<Image> = images
            .into_iter()
            .map(|r| r.expect("error not filtered out, check the logic"))
            .collect();

        send_as_photo_group(bot, target, images, caption).await?;
    }
    Ok(())
}

async fn send_as_document_group(
    bot: &tbot::Bot,
    target: ChatId<'_>,
    images: Vec<Image>,
    caption: Text<'_>,
) -> anyhow::Result<()> {
    assert!(!images.is_empty());
    let mut first = true;
    let group: Vec<GroupMedia> = images
        .iter()
        .map(|img| {
            if first {
                first = false;
                let doc = Document::with_bytes(&img.name, &img.data).caption(caption);
                todo!("tbot doesn't support ducoment as group")
            } else {
                let doc = Document::with_bytes(&img.name, &img.data);
                todo!("tbot doesn't support ducoment as group")
            }
        })
        .collect();
    bot.send_media_group(target, &group)
        .is_notification_disabled(true)
        .call()
        .await?;

    Ok(())
}

async fn send_as_photo_group(
    bot: &tbot::Bot,
    target: ChatId<'_>,
    images: Vec<Image>,
    caption: Text<'_>,
) -> anyhow::Result<()> {
    assert!(!images.is_empty());
    enum Or {
        Video(Vec<u8>),
        Photo(Vec<u8>),
    }
    let data: Vec<_> = images
        .into_iter()
        .map(|img| {
            if img.is_gif() {
                video_to_mp4(img.data).map(Or::Video)
            } else {
                Ok(Or::Photo(img.data))
            }
        })
        .collect::<Result<_, _>>()?;
    let mut first = true;
    let group: Vec<GroupMedia> = data
        .iter()
        .map(|d| match (d, first) {
            (Or::Video(v), true) => {
                first = false;
                Video::with_bytes(v).caption(caption).into()
            }
            (Or::Photo(p), true) => {
                first = false;
                Photo::with_bytes(p).caption(caption).into()
            }
            (Or::Video(v), false) => Video::with_bytes(v).into(),
            (Or::Photo(p), false) => Photo::with_bytes(p).into(),
        })
        .collect();
    bot.send_media_group(target, &group)
        .is_notification_disabled(true)
        .call()
        .await?;
    Ok(())
}

async fn send_the_old_way(
    bot: &tbot::Bot,
    target: ChatId<'_>,
    images: Vec<Result<Image, (anyhow::Error, &'_ str)>>,
    caption: Text<'_>,
) -> anyhow::Result<()> {
    for img_result in images {
        match img_result {
            Ok(img) => {
                if img.is_gif() {
                    let mp4 = video_to_mp4(img.data)?;
                    bot.send_video(target, Video::with_bytes(&mp4))
                        .is_notification_disabled(true)
                        .call()
                        .await?;
                } else {
                    if image_too_large(&img) {
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
                bot.send_message(target, &*img_url)
                    .is_notification_disabled(true)
                    .call()
                    .await?;
            }
        }

        tokio::time::delay_for(Duration::from_secs(3)).await;
    }
    bot.send_message(target, caption)
        .is_web_page_preview_disabled(true)
        .call()
        .await?;
    Ok(())
}

fn image_too_large(img: &Image) -> bool {
    std::cmp::max(img.width, img.height) > TG_IMAGE_DIMENSION_LIMIT
        && img.data.len() > LOW_QUALITY_IMG_SIZE
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
    for comment in &pic.comments.hot {
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
