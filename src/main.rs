extern crate jandan_pic_bot;

use jandan_pic_bot::*;

fn main() {
    let token = std::env::args().nth(1)
        .expect("Need a Telegram bot token as argument");
    let channel_id = std::env::args().nth(2)
        .expect("Please specify a Telegram Channel");
    println!("{} {}", token, channel_id);

    let data = spider::get_list().unwrap();
    println!("{:?}", data)
}
