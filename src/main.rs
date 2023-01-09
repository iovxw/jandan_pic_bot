#![feature(iter_intersperse)]

use std::borrow::Cow;
use std::fmt::Write;
use std::fs;
use std::time::Duration;

use anyhow::anyhow;
use image::GenericImageView;
use log::error;
use tbot::types::{
    input_file::{Animation, Photo},
    parameters::{ChatId, Text},
};

mod spider;
mod wayback_machine;

const HISTORY_SIZE: usize = 100;
const HISTORY_FILE: &str = "history.text";
const TG_IMAGE_SIZE_LIMIT: u32 = 1280;

async fn download_image(url: &str) -> anyhow::Result<image::DynamicImage> {
    let buf = spider::CLIENT
        .with(|client| client.get(url).header("referer", "https://jandan.net/"))
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let img = image::load_from_memory(&buf)?;
    Ok(img)
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

        for image in images {
            match async {
                if image.ends_with("gif") {
                    bot.send_animation(channel, Animation::with_url(&image))
                        .is_notification_disabled(true)
                        .call()
                        .await?;
                } else {
                    let img = download_image(&image).await?;
                    let caption = format!("[查看大图]({})", image);
                    let photo = if std::cmp::max(img.width(), img.height()) > TG_IMAGE_SIZE_LIMIT {
                        Photo::with_url(&image).caption(Text::with_markdown(&caption))
                    } else {
                        Photo::with_url(&image)
                    };
                    bot.send_photo(channel, photo)
                        .is_notification_disabled(true)
                        .call()
                        .await?;
                }
                Ok::<(), anyhow::Error>(())
            }
            .await
            {
                Ok(_) => {}
                Err(e) => {
                    error!("{}: {}", image, e);
                    bot.send_message(channel, &image)
                        .is_notification_disabled(true)
                        .call()
                        .await?;
                }
            }

            tokio::time::sleep(Duration::from_secs(3)).await;
        }

        let mut msg = format!(
            "*{}*: https://jandan.net/t/{}\n",
            &author.replace("*", ""),
            &id,
        );
        if !text.is_empty() {
            msg.push_str(&telegram_md_escape(&text));
            msg.push('\n');
        }
        write!(msg, "*OO*: {} *XX*: {}", oo, xx).unwrap();
        for comment in &comments {
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
        bot.send_message(channel, Text::with_markdown(&msg))
            .is_web_page_preview_disabled(true)
            .call()
            .await?;
        fresh_imgs.push(id.into());
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
