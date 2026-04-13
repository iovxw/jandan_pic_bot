use std::borrow::Cow;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{Cursor, Read, Write as _};
use std::time::Duration;

use convert::video_to_mp4;
use futures::prelude::*;
use log::error;
use teloxide::prelude::*;
use teloxide::requests::Requester;
use teloxide::types::{
    InputFile, InputMedia, InputMediaDocument, InputMediaPhoto, InputMediaVideo,
    LinkPreviewOptions, ParseMode, Recipient, ReplyParameters,
};

mod convert;
mod database;
mod http;
mod spider;
// mod wayback_machine;

const HISTORY_SOFT_LIMIT: usize = 100;
const HISTORY_FILE: &str = "history.text";
const TG_IMAGE_DIMENSION_LIMIT: u32 = 2560;
const TG_IMAGE_SIZE_LIMIT: usize = 10 * 1000 * 1000;
const LOW_QUALITY_IMG_SIZE: usize = 500 * 1024;
const TG_CAPTION_LIMIT: usize = 1024;

#[derive(Debug)]
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

/// Returns None if this conversion is a downgrade
fn upgrade_image_url(
    url: &str,
    require_large_image: bool,
    require_sinaimg: bool,
) -> Option<(Cow<'_, str>, &'static str)> {
    let is_large_image = url.contains("/large/");
    let is_sinaimg = url.contains("sinaimg.cn");
    if is_large_image && !require_large_image || is_sinaimg && !require_sinaimg {
        // don't downgrade
        return None;
    }
    let referer = if require_sinaimg {
        "https://weibo.com/"
    } else {
        "https://jandan.net/"
    };
    let mut url = Cow::from(url);
    if require_large_image && !is_large_image {
        url = Cow::from(
            url.replace("/mw1024/", "/large/")
                .replace("/mw600/", "/large/")
                .replace("/orj360/", "/large/"),
        );
    }
    if require_sinaimg && !is_sinaimg {
        url = Cow::from(
            url.replace("img.wangmoyu.com", "tva1.sinaimg.cn")
                .replace("img.toto.im", "tva1.sinaimg.cn")
                .replace("moyu.im", "sinaimg.cn"),
        );
    }

    Some((url, referer))
}

#[cfg(test)]
#[test]
fn test_upgrade_image_url() {
    assert_eq!(
        upgrade_image_url("https://img.toto.im/mw600/abcd.jpg", true, true)
            .unwrap()
            .0,
        "https://tva1.sinaimg.cn/large/abcd.jpg"
    );
    assert_eq!(
        upgrade_image_url("https://img.toto.im/mw600/abcd.jpg", true, false)
            .unwrap()
            .0,
        "https://img.toto.im/large/abcd.jpg"
    );
    assert_eq!(
        upgrade_image_url("https://img.toto.im/mw600/abcd.jpg", false, true)
            .unwrap()
            .0,
        "https://tva1.sinaimg.cn/mw600/abcd.jpg"
    );
    assert_eq!(
        upgrade_image_url("https://img.toto.im/mw600/abcd.jpg", false, false)
            .unwrap()
            .0,
        "https://img.toto.im/mw600/abcd.jpg"
    );
    assert!(upgrade_image_url("https://tva1.sinaimg.cn/large/abcd.jpg", true, true).is_some());
    assert!(upgrade_image_url("https://tva1.sinaimg.cn/large/abcd.jpg", false, false).is_none());
    assert!(upgrade_image_url("https://tva1.sinaimg.cn/large/abcd.jpg", true, false).is_none());
    assert!(upgrade_image_url("https://tva1.sinaimg.cn/large/abcd.jpg", false, true).is_none());
}

async fn download_image(url: &str) -> anyhow::Result<Image> {
    let mut errors = Vec::new();
    for &large_image in &[true, false] {
        for &sinaimg in &[true, false] {
            if let Some((url, referer)) = upgrade_image_url(url, large_image, sinaimg) {
                match download_image_with_referer(&url, referer).await {
                    Ok(image) => return Ok(image),
                    Err(e) => errors.push((url, e)),
                }
            }
        }
    }
    anyhow::bail!("Failed to download image from all candidates {:?}", errors);
}

async fn download_image_with_referer(url: &str, referer: &str) -> anyhow::Result<Image> {
    let file_name = reqwest::Url::parse(url)?
        .path_segments()
        .and_then(|mut s| s.next_back())
        .unwrap_or_default()
        .into();
    let resp = http::get_with_referer(url, referer).await?;
    let retrieved_filename = resp
        .url()
        .path_segments()
        .expect("not cannot-be-a-base URL")
        .next_back()
        .expect("always has one path segment");
    if retrieved_filename.starts_with("default_") {
        anyhow::bail!("夹");
    }
    let buf = resp.error_for_status()?.bytes().await?;
    let reader = image::ImageReader::new(Cursor::new(&buf))
        .with_guessed_format()
        .expect("io read error in Cursor<Vec>?");
    let format = reader.format().ok_or_else(|| {
        image::ImageError::Unsupported(image::error::ImageFormatHint::Unknown.into())
    })?;
    let dimensions = reader.into_dimensions()?;
    Ok(Image {
        format,
        name: file_name,
        width: dimensions.0,
        height: dimensions.1,
        data: buf.to_vec(),
    })
}

// TODO: CoW
fn telegram_md_escape(s: &str) -> String {
    let mut r = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '_' | '*'
                | '['
                | ']'
                | '('
                | ')'
                | '~'
                | '`'
                | '>'
                | '#'
                | '+'
                | '-'
                | '='
                | '|'
                | '{'
                | '}'
                | '.'
                | '!'
                | '\\'
        ) {
            r.push('\\');
        }
        r.push(c);
    }
    r
}

fn telegram_md_escape_url(s: &str) -> String {
    s.replace('\\', "\\\\").replace(')', "\\)")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    let mut db = database::Database::open("db.json").await?;
    let bot = teloxide::Bot::new(db.token.clone());
    let mut history_file = File::options().read(true).append(true).open(HISTORY_FILE)?;

    let mut buf = String::new();
    history_file.read_to_string(&mut buf)?;
    let mut history: Vec<&str> = buf.lines().filter(|l| !l.is_empty()).collect();
    let mut new_pics = Vec::new();

    let pics = spider::do_the_evil().await?;
    for pic in pics.into_iter().filter(|pic| !history.contains(&&*pic.id)) {
        upload_comment_images(&bot, &mut db, &pic.comments).await?;
        upload_comment_mentions(&bot, &mut db, &pic.comments).await?;
        send_pic(&bot, &db, &pic).await?;

        write!(history_file, "\n{}", pic.id)?;
        new_pics.push(pic.id);
    }
    history.extend(new_pics.iter().map(String::as_str));
    let fresh_start = history.len().saturating_sub(HISTORY_SOFT_LIMIT);
    // truncate history
    // TODO: FALLOC_FL_COLLAPSE_RANGE
    std::fs::write(HISTORY_FILE, history[fresh_start..].join("\n"))?;

    // let wayback_machine_token = std::env::args().nth(1);
    // if let Some(token) = wayback_machine_token {
    //     wayback_machine::push(&token, &fresh_imgs).await?;
    // }
    Ok(())
}

async fn send_pic(
    bot: &teloxide::Bot,
    db: &database::Database,
    pic: &spider::Pic,
) -> anyhow::Result<()> {
    let images: Vec<Result<Image, (_, &str)>> = futures::stream::iter(&pic.images)
        .then(|url| download_image(url).map_err(|e| (e, url.as_str())))
        .collect()
        .await;

    let mut captions: Vec<String> = format_caption(db, pic);
    let contains_error = images.iter().any(|r| r.is_err());
    let contains_large_image = images
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .any(image_too_large);
    let contains_gif = images
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .any(|img| img.is_gif());
    if images.is_empty() || contains_error || contains_large_image && contains_gif {
        send_the_old_way(bot, db.channel(), images, captions).await?;
        return Ok(());
    }
    assert!(!images.is_empty());
    if contains_large_image {
        assert!(!contains_gif);
        // TODO: replace with:
        // send_as_document_group(bot, target, images, captions).await?;
        if images.len() == 1 {
            let img: Image = images.into_iter().find_map(|x| x.ok()).unwrap();
            let caption = captions.remove(0);
            let doc = InputFile::memory(img.data).file_name(img.name);
            let first_msg = bot
                .send_document(db.channel(), doc)
                .caption(caption)
                .parse_mode(ParseMode::MarkdownV2)
                .disable_notification(true)
                .await?;
            for caption in captions {
                bot.send_message(db.channel(), caption)
                    .parse_mode(ParseMode::MarkdownV2)
                    .link_preview_options(LinkPreviewOptions {
                        is_disabled: true,
                        url: None,
                        prefer_small_media: false,
                        prefer_large_media: false,
                        show_above_text: false,
                    })
                    .reply_parameters(ReplyParameters::new(first_msg.id))
                    .await?;
            }
        } else {
            send_the_old_way(bot, db.channel(), images, captions).await?;
        }
    } else {
        let images: Vec<Image> = images
            .into_iter()
            .map(|r| r.expect("error not filtered out, check the logic"))
            .collect();

        send_as_photo_group(bot, db.channel(), images, captions).await?;
    }
    Ok(())
}

#[allow(unused)]
async fn send_as_document_group(
    bot: &teloxide::Bot,
    target: Recipient,
    images: Vec<Image>,
    caption: String,
) -> anyhow::Result<()> {
    assert!(!images.is_empty());
    let group: Vec<InputMedia> = images
        .into_iter()
        .enumerate()
        .map(|(i, img)| {
            let doc = InputMediaDocument::new(InputFile::memory(img.data).file_name(img.name));
            if i == 0 {
                InputMedia::Document(
                    doc.caption(caption.clone())
                        .parse_mode(ParseMode::MarkdownV2),
                )
            } else {
                InputMedia::Document(doc)
            }
        })
        .collect();
    bot.send_media_group(target, group)
        .disable_notification(true)
        .await?;

    Ok(())
}

async fn send_as_photo_group(
    bot: &teloxide::Bot,
    target: Recipient,
    images: Vec<Image>,
    mut captions: Vec<String>,
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
    let caption = captions.remove(0);
    let group: Vec<InputMedia> = data
        .into_iter()
        .enumerate()
        .map(|(i, d)| {
            let (file, is_video) = match d {
                Or::Video(v) => (InputFile::memory(v), true),
                Or::Photo(p) => (InputFile::memory(p), false),
            };
            if is_video {
                let mut m = InputMediaVideo::new(file);
                if i == 0 {
                    m = m.caption(caption.clone()).parse_mode(ParseMode::MarkdownV2);
                }
                InputMedia::Video(m)
            } else {
                let mut m = InputMediaPhoto::new(file);
                if i == 0 {
                    m = m.caption(caption.clone()).parse_mode(ParseMode::MarkdownV2);
                }
                InputMedia::Photo(m)
            }
        })
        .collect();
    let first_msg = bot
        .send_media_group(target.clone(), group)
        .disable_notification(true)
        .await?;
    let first_msg_id = first_msg.first().expect("tg return 0 msg").id;
    for caption in captions {
        bot.send_message(target.clone(), caption)
            .parse_mode(ParseMode::MarkdownV2)
            .link_preview_options(LinkPreviewOptions {
                is_disabled: true,
                url: None,
                prefer_small_media: false,
                prefer_large_media: false,
                show_above_text: false,
            })
            .reply_parameters(ReplyParameters::new(first_msg_id))
            .await?;
    }

    Ok(())
}

async fn upload_single_image(
    bot: &teloxide::Bot,
    target: Recipient,
    img: Image,
) -> anyhow::Result<teloxide::types::Message> {
    let msg = if img.is_gif() {
        let mp4 = video_to_mp4(img.data)?;
        bot.send_video(target, InputFile::memory(mp4))
            .disable_notification(true)
            .await?
    } else if image_too_large(&img) {
        bot.send_document(target, InputFile::memory(img.data).file_name(img.name))
            .disable_notification(true)
            .await?
    } else {
        bot.send_photo(target, InputFile::memory(img.data))
            .disable_notification(true)
            .await?
    };
    Ok(msg)
}

async fn send_the_old_way(
    bot: &teloxide::Bot,
    target: Recipient,
    images: Vec<Result<Image, (anyhow::Error, &'_ str)>>,
    mut captions: Vec<String>,
) -> anyhow::Result<()> {
    for img_result in images {
        match img_result {
            Ok(img) => {
                upload_single_image(bot, target.clone(), img).await?;
            }
            Err((e, img_url)) => {
                error!("{}: {}", img_url, e);
                bot.send_message(target.clone(), img_url)
                    .disable_notification(true)
                    .await?;
            }
        }

        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    let caption = captions.remove(0);
    let first_msg = bot
        .send_message(target.clone(), caption)
        .parse_mode(ParseMode::MarkdownV2)
        .link_preview_options(LinkPreviewOptions {
            is_disabled: true,
            url: None,
            prefer_small_media: false,
            prefer_large_media: false,
            show_above_text: false,
        })
        .await?;
    for caption in captions {
        bot.send_message(target.clone(), caption)
            .parse_mode(ParseMode::MarkdownV2)
            .link_preview_options(LinkPreviewOptions {
                is_disabled: true,
                url: None,
                prefer_small_media: false,
                prefer_large_media: false,
                show_above_text: false,
            })
            .reply_parameters(ReplyParameters::new(first_msg.id))
            .await?;
    }
    Ok(())
}

fn image_too_large(img: &Image) -> bool {
    !img.is_gif()
        && ((std::cmp::max(img.width, img.height) > TG_IMAGE_DIMENSION_LIMIT
            && img.data.len() > LOW_QUALITY_IMG_SIZE)
            || img.data.len() > TG_IMAGE_SIZE_LIMIT)
}

fn format_caption(db: &database::Database, pic: &spider::Pic) -> Vec<String> {
    let mut msg = format!(
        "*{}*: [jandan\\.net/t/{}](https://jandan.net/t/{})\n",
        telegram_md_escape(&pic.author.replace('*', "")),
        telegram_md_escape(&pic.id),
        telegram_md_escape_url(&pic.id),
    );
    if !pic.text.is_empty() {
        msg.push_str(&telegram_md_escape(&pic.text));
        msg.push('\n');
    }
    write!(msg, "*OO*: {} *XX*: {}", pic.oo, pic.xx).unwrap();
    let mut msgs = vec![msg];
    for comment in &pic.comments.hot {
        let msg = msgs.last_mut().expect("never");
        let formatted = format!(
            "\n*{}*: {}\n*OO*: {}, *XX*: {}",
            telegram_md_escape(&comment.author.replace('*', "")),
            comment_to_tg_md(db, &comment.content),
            comment.oo,
            comment.xx
        );
        if msg.chars().count() + formatted.chars().count() > TG_CAPTION_LIMIT {
            msgs.push(formatted);
        } else {
            msg.push_str(&formatted);
        }
    }
    msgs
}

fn comment_to_tg_md(db: &database::Database, comment: &spider::RichText) -> String {
    let mut r = String::new();
    for e in comment.entities() {
        use spider::TextEntity::*;
        match e {
            Text(s) => r.push_str(&telegram_md_escape(s)),
            Br => r.push('\n'),
            Img(url) => {
                if let Some(tg_link) = db.get_img(url) {
                    write!(r, "[［图片］]({})", telegram_md_escape_url(&tg_link))
                        .expect("never fail");
                } else {
                    r.push_str(&telegram_md_escape(url))
                }
            }
            Mention { name, id } => {
                if let Some(msg_link) = db.get_comment(id) {
                    write!(
                        r,
                        "[{}]({})",
                        telegram_md_escape(name),
                        telegram_md_escape_url(&msg_link)
                    )
                    .expect("never fail");
                } else {
                    r.push_str(&telegram_md_escape(name))
                }
            }
        }
    }
    r.trim().to_string() // TODO: zero alloc?
}

async fn upload_comment_images(
    bot: &teloxide::Bot,
    db: &mut database::Database,
    c: &spider::Comments,
) -> Result<(), anyhow::Error> {
    for comment in c
        .hot
        .iter()
        .chain(c.mentions.values().filter_map(|c| c.as_ref()))
    {
        for entry in comment.content.entities() {
            if let spider::TextEntity::Img(url) = entry {
                if db.get_img(url).is_some() {
                    continue;
                }
                match download_image(url).await {
                    Ok(img) => {
                        let msg = upload_single_image(bot, db.assets_channel(), img).await?;
                        db.put_img(url.to_string(), msg.id.0 as u64).await;
                    }
                    Err(e) => {
                        error!("{}: {}", url, e);
                        let msg = bot
                            .send_message(db.assets_channel(), url)
                            .disable_notification(true)
                            .await?;
                        db.put_img(url.to_string(), msg.id.0 as u64).await;
                    }
                }
            }
        }
    }
    Ok(())
}

async fn upload_comment_mentions(
    bot: &teloxide::Bot,
    db: &mut database::Database,
    c: &spider::Comments,
) -> Result<(), anyhow::Error> {
    for (&id, comment) in &c.mentions {
        if db.get_comment(id).is_some() {
            continue;
        }
        let text = if let Some(comment) = comment {
            format!(
                "*{}*: {}\n*OO*: {}, *XX*: {}",
                telegram_md_escape(&comment.author.replace('*', "")),
                comment_to_tg_md(db, &comment.content),
                comment.oo,
                comment.xx
            )
        } else {
            "这条吐槽不见了".into()
        };

        let msg = bot
            .send_message(db.assets_channel(), text)
            .parse_mode(ParseMode::MarkdownV2)
            .disable_notification(true)
            .await?;
        db.put_comment(id, msg.id.0 as u64).await;
    }
    Ok(())
}
