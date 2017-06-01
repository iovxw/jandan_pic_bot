error_chain! {
    foreign_links {
        Io(::std::io::Error);
        Net(::tokio_curl::PerformError);
        Json(::serde_json::error::Error);
        Num(::std::num::ParseIntError);
    }
}
