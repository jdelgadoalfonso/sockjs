use std::marker::PhantomData;
use std::time::Duration;

use actix::*;
use actix_web::http::Method;
use actix_web::*;
use regex::Regex;
use serde_json;

use context::ChannelItem;
use manager::{Broadcast, Record, SessionManager};
use protocol::{CloseCode, Frame};
use session::Session;
use utils::SockjsHeaders;

use super::{Flags, SendResult, Transport};

const PRELUDE1: &str = r#"
<!doctype html>
<html><head>
  <meta http-equiv="X-UA-Compatible" content="IE=edge" />
  <meta http-equiv="Content-Type" content="text/html; charset=UTF-8" />
</head><body><h2>Don't panic!</h2>
  <script>
    document.domain = document.domain;
    var c = parent."#;

const PRELUDE2: &str = r#";
    c.start();
    function p(d) {c.message(d);};
    window.onload = function() {c.stop();};
  </script>"#;

const PRELUDE3: &[u8] = &[b' '; 1024];

pub struct HTMLFile<S, SM>
where
    S: Session,
    SM: SessionManager<S>,
{
    s: PhantomData<S>,
    sm: PhantomData<SM>,
    size: usize,
    maxsize: usize,
    rec: Option<Record>,
    flags: Flags,
}

impl<S, SM> HTMLFile<S, SM>
where
    S: Session,
    SM: SessionManager<S>,
{
    // start heartbeats
    fn hb(&self, ctx: &mut HttpContext<Self, Addr<SM>>) {
        ctx.run_later(Duration::new(5, 0), |act, ctx| {
            act.send_heartbeat(ctx);
            act.hb(ctx);
        });
    }

    fn write(&mut self, s: &str, ctx: &mut HttpContext<Self, Addr<SM>>) {
        let b = serde_json::to_string(s).unwrap();
        self.size += b.len() + 25;
        ctx.write("<script>\np(");
        ctx.write(b);
        ctx.write(");\n</script>\r\n");
    }

    pub fn init(req: HttpRequest<Addr<SM>>, maxsize: usize) -> Result<HttpResponse> {
        lazy_static! {
            static ref CHECK: Regex = Regex::new(r"^[a-zA-Z0-9_\.]+$").unwrap();
        }
        if *req.method() != Method::GET {
            return Ok(HttpResponse::NotFound().into());
        }

        if let Some(callback) = req.query().get("c").map(|s| s.to_owned()) {
            if !CHECK.is_match(&callback) {
                return Ok(
                    HttpResponse::InternalServerError().body("invalid \"callback\" parameter")
                );
            }

            let session = req.match_info().get("session").unwrap().to_owned();
            let mut resp = HttpResponse::Ok()
                .force_close()
                .no_chunking()
                .content_type("text/html; charset=UTF-8")
                .sockjs_no_cache()
                .sockjs_session_cookie(&req)
                .take();

            let mut ctx = HttpContext::new(
                req,
                HTMLFile {
                    s: PhantomData,
                    sm: PhantomData,
                    size: 0,
                    rec: None,
                    maxsize,
                    flags: Flags::empty(),
                },
            );
            ctx.write(PRELUDE1);
            ctx.write(callback);
            ctx.write(PRELUDE2);
            ctx.write(PRELUDE3);

            // init transport, but aftre prelude only
            ctx.drain()
                .map(move |_, _, ctx| {
                    ctx.run_later(Duration::new(0, 1_200_000), move |act, ctx| {
                        act.hb(ctx);
                        act.init_transport(session, ctx);
                    });
                }).wait(&mut ctx);

            Ok(resp.body(ctx))
        } else {
            Ok(HttpResponse::InternalServerError().body("\"callback\" parameter required"))
        }
    }
}

// Http actor implementation
impl<S, SM> Actor for HTMLFile<S, SM>
where
    S: Session,
    SM: SessionManager<S>,
{
    type Context = HttpContext<Self, Addr<SM>>;

    fn stopping(&mut self, ctx: &mut Self::Context) -> Running {
        self.release(ctx);
        Running::Stop
    }
}

// Transport implementation
impl<S, SM> Transport<S, SM> for HTMLFile<S, SM>
where
    S: Session,
    SM: SessionManager<S>,
{
    fn send(&mut self, ctx: &mut Self::Context, msg: &Frame, rec: &mut Record) -> SendResult {
        match *msg {
            Frame::Heartbeat => {
                self.write("h", ctx);
            }
            Frame::Message(ref s) => {
                let blob = format!("a[{}]", serde_json::to_string(&s).unwrap());
                self.write(&blob, ctx);
            }
            Frame::MessageVec(ref s) => {
                self.write(s, ctx);
            }
            Frame::MessageBlob(_) => unimplemented!(),
            Frame::Open => {
                self.write("o", ctx);
            }
            Frame::Close(code) => {
                rec.close();
                let blob = format!("c[{},{:?}]", code.num(), code.reason());
                self.write(&blob, ctx);
                ctx.write_eof();
                return SendResult::Stop;
            }
        };

        if self.size > self.maxsize {
            ctx.write_eof();
            SendResult::Stop
        } else {
            SendResult::Continue
        }
    }

    fn send_close(&mut self, ctx: &mut Self::Context, code: CloseCode) {
        self.write(&format!("c[{},{:?}]", code.num(), code.reason()), ctx);
    }

    fn send_heartbeat(&mut self, ctx: &mut Self::Context) {
        self.write("h", ctx);
    }

    fn session_record(&mut self) -> &mut Option<Record> {
        &mut self.rec
    }

    fn flags(&mut self) -> &mut Flags {
        &mut self.flags
    }
}

impl<S, SM> Handler<ChannelItem> for HTMLFile<S, SM>
where
    S: Session,
    SM: SessionManager<S>,
{
    type Result = ();

    fn handle(&mut self, msg: ChannelItem, ctx: &mut Self::Context) {
        self.handle_message(msg, ctx)
    }
}

impl<S, SM> Handler<Broadcast> for HTMLFile<S, SM>
where
    S: Session,
    SM: SessionManager<S>,
{
    type Result = ();

    fn handle(&mut self, msg: Broadcast, ctx: &mut Self::Context) {
        if let Some(mut rec) = self.rec.take() {
            self.send(ctx, &msg.msg, &mut rec);
            self.rec = Some(rec);
        } else if let Some(ref mut rec) = self.rec {
            rec.add(msg);
        }
    }
}
