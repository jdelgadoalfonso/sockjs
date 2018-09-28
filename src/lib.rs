//! [`SockJS`](https://github.com/sockjs/sockjs-client) server
//! for [Actix](https://github.com/actix/actix)

#[macro_use]
extern crate log;
extern crate bytes;
extern crate futures;
extern crate md5;
extern crate rand;
extern crate regex;
extern crate time;
#[macro_use]
extern crate lazy_static;
extern crate percent_encoding;
#[macro_use]
extern crate bitflags;

extern crate serde;
extern crate serde_json;
#[macro_use]
extern crate serde_derive;

extern crate http;
#[macro_use]
extern crate actix;
extern crate actix_web;

mod application;
mod context;
mod manager;
mod protocol;
mod session;
mod transports;
mod utils;

pub use application::SockJS;
pub use context::SockJSContext;
pub use manager::SockJSManager;
pub use session::{CloseReason, Message, Session};
