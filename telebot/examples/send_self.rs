extern crate futures;
extern crate telebot;
extern crate tokio_core;

use telebot::RcBot;
use tokio_core::reactor::Core;
use futures::stream::Stream;
use std::env;

// import all available functions
use telebot::functions::*;

fn main() {
    // Create a new tokio core
    let mut lp = Core::new().unwrap();

    // Create the bot
    let bot = RcBot::new(lp.handle(), &env::var("TELEGRAM_BOT_KEY").unwrap()).update_interval(200);

    let handle = bot.new_cmd("/send_self")
        .and_then(|(bot, msg)| {
            bot.document(msg.chat.id)
                .file("examples/send_self.rs")
                .send()
        })
        .map_err(|err| println!("{:?}", err.cause()));

    bot.register(handle);

    // enter the main loop
    bot.run(&mut lp).unwrap();
}
