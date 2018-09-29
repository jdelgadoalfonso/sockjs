use std::marker::PhantomData;
use std::sync::Arc;

use actix::*;
use actix_web::*;
use rand;

use context::ChannelItem;
use manager::{Acquire, Broadcast, Record, Release, SessionManager, SessionMessage};
use protocol::{CloseCode, Frame};
use session::{Message, Session, SessionState};

use super::{Flags, SendResult};

pub struct RawWebsocket<S, SM>
where
    S: Session,
    SM: SessionManager<S>,
{
    s: PhantomData<S>,
    sm: PhantomData<SM>,
    rec: Option<Record>,
    flags: Flags,
}

impl<S, SM> RawWebsocket<S, SM>
where
    S: Session,
    SM: SessionManager<S>,
{
    pub fn init(req: HttpRequest<Addr<SM>>) -> Result<HttpResponse> {
        let mut resp = ws::handshake(&req)?;

        // session
        let sid = format!("{}", rand::random::<u32>());

        let mut ctx = ws::WebsocketContext::from_request(req.clone());
        ctx.add_stream(ws::WsStream::new(req));
        // let mut ctx = ws::WebsocketContext::create(req, ws::WsStream::new(req), resp);
        let mut tr = RawWebsocket {
            s: PhantomData,
            sm: PhantomData,
            rec: None,
            flags: Flags::empty(),
        };
        // init transport
        tr.init_transport(sid, &mut ctx);

        Ok(resp.body(ctx.actor(tr)))
    }

    fn send(
        &mut self,
        ctx: &mut ws::WebsocketContext<Self, Addr<SM>>,
        msg: &Frame,
        record: &mut Record,
    ) -> SendResult {
        match *msg {
            Frame::Heartbeat => {
                ctx.ping("");
            }
            Frame::Message(ref s) | Frame::MessageVec(ref s) => {
                ctx.text(s);
            }
            Frame::MessageBlob(ref b) => {
                ctx.binary(b.clone());
            }
            Frame::Open => (),
            Frame::Close(_) => {
                record.close();
                ctx.close(Some(ws::CloseReason {
                    code: ws::CloseCode::Normal,
                    description: Some("Go away!".to_owned()),
                }));
            }
        };

        SendResult::Continue
    }

    fn send_close(&mut self, ctx: &mut ws::WebsocketContext<Self, Addr<SM>>, _: CloseCode) {
        ctx.close(Some(ws::CloseReason {
            code: ws::CloseCode::Normal,
            description: Some("Go away!".to_owned()),
        }));
    }

    fn session_record(&mut self) -> &mut Option<Record> {
        &mut self.rec
    }

    fn flags(&mut self) -> &mut Flags {
        &mut self.flags
    }

    /// Stop transport and release session
    fn release(&mut self, ctx: &mut ws::WebsocketContext<Self, Addr<SM>>) {
        if let Some(mut rec) = self.session_record().take() {
            if !ctx.connected() {
                rec.interrupted();
            }
            ctx.state().do_send(Release { ses: rec });
        }
        ctx.stop();
    }

    fn handle_message(&mut self, msg: ChannelItem, ctx: &mut ws::WebsocketContext<Self, Addr<SM>>) {
        match msg {
            ChannelItem::Frame(msg) => {
                if let Some(mut rec) = self.session_record().take() {
                    if self.flags().contains(Flags::READY) {
                        if SendResult::Stop == self.send(ctx, &msg, &mut rec) {
                            *self.session_record() = Some(rec);
                            self.release(ctx);
                        } else {
                            *self.session_record() = Some(rec);
                        }
                    } else {
                        rec.add(msg);
                        *self.session_record() = Some(rec);
                    }
                }
            }
            ChannelItem::Ready => {
                if let Some(mut rec) = self.session_record().take() {
                    if SendResult::Stop == self.send_buffered(ctx, &mut rec) {
                        *self.session_record() = Some(rec);
                        self.release(ctx);
                    } else {
                        *self.session_record() = Some(rec);
                    }
                }
                if self.flags().contains(Flags::RELEASE) {
                    self.release(ctx)
                } else {
                    self.flags().insert(Flags::READY);
                }
            }
        }
    }

    /// Send sockjs frame
    fn send_buffered(
        &mut self,
        ctx: &mut ws::WebsocketContext<Self, Addr<SM>>,
        record: &mut Record,
    ) -> SendResult {
        while !record.buffer.is_empty() {
            if let Some(msg) = record.buffer.pop_front() {
                if let SendResult::Stop = self.send(ctx, msg.as_ref(), record) {
                    return SendResult::Stop;
                }
            }
        }
        SendResult::Continue
    }

    fn init_transport(&mut self, session: String, ctx: &mut ws::WebsocketContext<Self, Addr<SM>>) {
        // acquire session
        let addr: Addr<_> = ctx.address();
        ctx.state().send(Acquire::new(session, addr.recipient()))
            .into_actor(self)
            .map(|res, act, ctx| {
                match res {
                    Ok(mut rec) => {
                        // copy messages into buffer
                        trace!("STATE: {:?}", rec.0.state);

                        match rec.0.state {
                            SessionState::Running => {
                                if let SendResult::Stop = act.send_buffered(ctx, &mut rec.0) {
                                    // release immidietly
                                    act.flags().insert(Flags::RELEASE);
                                }
                                *act.session_record() = Some(rec.0);
                                ctx.add_message_stream(rec.1);
                            },
                            SessionState::New => {
                                rec.0.state = SessionState::Running;
                                if let SendResult::Stop = act.send(ctx, &Frame::Open, &mut rec.0)
                                {
                                    // release is send stops
                                    act.flags().insert(Flags::RELEASE);
                                } else if let SendResult::Stop =
                                    act.send_buffered(ctx, &mut rec.0) // send buffered messages
                                {
                                    // release immidietly
                                    act.flags().insert(Flags::RELEASE);
                                }
                                *act.session_record() = Some(rec.0);
                                ctx.add_message_stream(rec.1);
                            },

                            SessionState::Interrupted => {
                                act.send(ctx, &Frame::Close(CloseCode::Interrupted), &mut rec.0);
                                ctx.state().do_send(Release{ses: rec.0});
                            },

                            SessionState::Closed => {
                                act.send(ctx, &Frame::Close(CloseCode::GoAway), &mut rec.0);
                                ctx.state().do_send(Release{ses: rec.0});
                            }
                        }
                    },
                    Err(err) => {
                        act.send_close(ctx, err.into());
                        ctx.stop();
                    }
                }
            })
            // session manager is dead?
            .map_err(|_, act, ctx| {
                act.send_close(ctx, CloseCode::InternalError);
            })
            .wait(ctx);
    }
}

// Http actor implementation
impl<S, SM> Actor for RawWebsocket<S, SM>
where
    S: Session,
    SM: SessionManager<S>,
{
    type Context = ws::WebsocketContext<Self, Addr<SM>>;

    fn stopping(&mut self, ctx: &mut Self::Context) -> Running {
        if let Some(mut rec) = self.rec.take() {
            rec.close();
            ctx.state().do_send(Release { ses: rec });
        }
        Running::Stop
    }
}

impl<S, SM> Handler<ChannelItem> for RawWebsocket<S, SM>
where
    S: Session,
    SM: SessionManager<S>,
{
    type Result = ();

    fn handle(&mut self, msg: ChannelItem, ctx: &mut Self::Context) {
        self.handle_message(msg, ctx)
    }
}

impl<S, SM> Handler<Broadcast> for RawWebsocket<S, SM>
where
    S: Session,
    SM: SessionManager<S>,
{
    type Result = ();

    fn handle(&mut self, msg: Broadcast, ctx: &mut Self::Context) {
        if let Some(mut rec) = self.rec.take() {
            self.send(ctx, &msg.msg, &mut rec);
            self.rec = Some(rec);
        }
    }
}

impl<S, SM> StreamHandler<ws::Message, ws::ProtocolError> for RawWebsocket<S, SM>
where
    S: Session,
    SM: SessionManager<S>,
{
    fn error(&mut self, _: ws::ProtocolError, ctx: &mut Self::Context) -> Running {
        if let Some(rec) = self.rec.take() {
            ctx.state().do_send(Release { ses: rec });
        }
        self.release(ctx);
        Running::Stop
    }

    fn handle(&mut self, msg: ws::Message, ctx: &mut Self::Context) {
        // process websocket messages
        match msg {
            ws::Message::Ping(msg) => ctx.pong(&msg),
            ws::Message::Text(text) => {
                if !text.is_empty() {
                    if let Some(ref rec) = self.rec {
                        ctx.state().do_send(SessionMessage {
                            sid: Arc::clone(&rec.sid),
                            msg: Message(text),
                        });
                    }
                }
            }
            ws::Message::Binary(_) => {
                error!("Not supported!");
            }
            _ => (),
        }
    }
}
