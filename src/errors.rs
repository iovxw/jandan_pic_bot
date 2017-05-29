error_chain! {
    foreign_links {
        Io(::std::io::Error);
        Net(::curl::Error);
        Json(::serde_json::error::Error);
        Num(::std::num::ParseIntError);
    }
}
