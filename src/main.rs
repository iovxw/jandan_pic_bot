use std::borrow::Cow;
use std::fmt::Write;
use std::fs;

use anyhow::anyhow;
use image::{self, GenericImageView};
use itertools::Itertools;
use log::error;
use tbot::{
    self,
    types::{
        input_file::{Animation, Photo},
        parameters::{ChatId, NotificationState, Text, WebPagePreviewState},
    },
};
use tokio;

mod spider;

const HISTORY_SIZE: usize = 100;
const HISTORY_FILE: &str = "history.text";
const TG_IMAGE_SIZE_LIMIT: u32 = 1280;

async fn download_image(url: &str) -> anyhow::Result<image::DynamicImage> {
    let buf = spider::CLIENT
        .with(|client| client.get(url))
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

    let bot = tbot::Bot::new(token);
    let channel: ChatId = channel_id.as_str().into();
    let history = fs::read_to_string(HISTORY_FILE)?;
    let history: Vec<&str> = history.lines().collect();
    let pics = spider::do_the_evil().await?;
    let mut new_history: Vec<Cow<str>> = Vec::with_capacity(HISTORY_SIZE);

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
                    bot.send_animation(channel, Animation::url(&image))
                        .notification(NotificationState::Disabled)
                        .call()
                        .await?;
                } else {
                    let img = download_image(&image).await?;
                    let caption = format!("_[查看大图]({})_", image);
                    let photo = if std::cmp::max(img.width(), img.height()) > TG_IMAGE_SIZE_LIMIT {
                        Photo::url(&image).caption(Text::markdown(&caption))
                    } else {
                        Photo::url(&image)
                    };
                    bot.send_photo(channel, photo)
                        .notification(NotificationState::Disabled)
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
                        .notification(NotificationState::Disabled)
                        .call()
                        .await?;
                }
            }
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
        bot.send_message(channel, Text::markdown(&msg))
            .web_page_preview(WebPagePreviewState::Disabled)
            .call()
            .await?;
        new_history.push(id.into());
    }
    new_history.extend(
        history
            .into_iter()
            .map(Into::into)
            .take(HISTORY_SIZE - new_history.len()),
    );
    fs::write(
        HISTORY_FILE,
        new_history
            .into_iter()
            .intersperse("\n".into())
            .collect::<String>(),
    )?;
    Ok(())
}
