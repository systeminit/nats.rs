use std::collections::VecDeque;
use std::io::{self, Error, ErrorKind};
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::channel::{mpsc, oneshot};
use futures::prelude::*;

use crate::inject_io_failure;
use crate::new_client::encoder::{encode, ClientOp};
use crate::new_client::message::Message;

pub(crate) struct Writer {
    pub(crate) stream: Option<Pin<Box<dyn AsyncWrite + Send>>>,

    bytes: Box<[u8]>,
    written: usize,
    committed: usize,

    /// Next subscription ID.
    next_sid: u64,

    /// Current subscriptions in the form `(subject, sid, messages)`.
    subscriptions: Vec<(String, u64, mpsc::UnboundedSender<Message>)>,

    /// Expected pongs and their notification channels.
    pongs: VecDeque<oneshot::Sender<()>>,
}

impl Writer {
    pub(crate) fn new(buf_size: usize) -> Writer {
        Writer {
            stream: None,
            bytes: vec![0u8; buf_size].into_boxed_slice(),
            written: 0,
            committed: 0,
            next_sid: 1,
            subscriptions: Vec::new(),
            pongs: VecDeque::new(),
        }
    }

    pub(crate) async fn reconnect(
        &mut self,
        mut stream: impl AsyncWrite + Send + 'static,
    ) -> io::Result<()> {
        // Drop the current stream, if there is any.
        self.stream = None;

        let mut stream = Box::pin(stream);

        // Restart subscriptions that existed before the last reconnect.
        // TODO(stjepang): remove this clone()
        for (subject, sid, _messages) in self.subscriptions.clone().iter() {
            // Send a SUB operation to the server.
            encode(
                &mut stream,
                ClientOp::Sub {
                    subject: subject.as_str(),
                    queue_group: None, // TODO(stjepang): Use actual queue group here.
                    sid: *sid,
                },
            )
            .await?;
        }

        // Take out buffered operations.
        let range = ..self.committed;
        self.written = 0;
        self.committed = 0;

        // Inject random I/O failures when testing.
        inject_io_failure()?;

        // Write buffered operations into the new stream.
        stream.write_all(&self.bytes[range]).await?;
        stream.flush().await?;

        // Use the new stream from now on.
        self.stream = Some(stream);
        Ok(())
    }

    /// Sends a PING to the server.
    pub(crate) async fn ping(&mut self) -> io::Result<oneshot::Receiver<()>> {
        encode(&mut *self, ClientOp::Ping).await?;
        self.commit();

        // Flush to make sure the PING goes through.
        self.flush().await?;

        let (sender, receiver) = oneshot::channel();
        self.pongs.push_back(sender);

        Ok(receiver)
    }

    pub(crate) async fn subscribe(
        &mut self,
        subject: &str,
        queue_group: Option<&str>,
    ) -> io::Result<(u64, mpsc::UnboundedReceiver<Message>)> {
        let sid = self.next_sid;
        self.next_sid += 1;

        self.send(ClientOp::Sub {
            subject,
            queue_group,
            sid,
        })
        .await;

        // Add the subscription to the list.
        let (s, r) = mpsc::unbounded();
        self.subscriptions.push((subject.to_string(), sid, s));
        Ok((sid, r))
    }

    pub(crate) async fn unsubscribe(&mut self, sid: u64) -> io::Result<()> {
        self.send(ClientOp::Unsub {
            sid,
            max_msgs: None, // TODO(stjepang): fill this out
        })
        .await;

        Ok(())
    }

    pub(crate) async fn publish(
        &mut self,
        subject: &str,
        reply_to: Option<&str>,
        msg: &[u8],
    ) -> io::Result<()> {
        encode(
            &mut *self,
            ClientOp::Pub {
                subject,
                reply_to,
                payload: msg,
            },
        )
        .await?;
        self.commit();

        Ok(())
    }

    /// Processes a PONG received from the server.
    pub(crate) fn pong(&mut self) {
        // If a pong is received while disconnected, it came from a connection that isn't alive
        // anymore and therefore doesn't correspond to the next expected pong.
        if self.stream.is_some() {
            // Take the next expected pong from the queue.
            let pong = self.pongs.pop_front().expect("unexpected pong");

            // Complete the pong by sending a message into the channel.
            let _ = pong.send(());
        }
    }

    pub(crate) fn message(
        &mut self,
        subject: String,
        sid: u64,
        reply_to: Option<String>,
        payload: Vec<u8>,
    ) {
        // Send the message to matching subscription.
        if let Some((_, _, messages)) = self.subscriptions.iter().find(|(_, s, _)| *s == sid) {
            // TODO(stjepang): If sending a message errors, unsubscribe and remove this channel.
            let _ = messages.unbounded_send(Message {
                subject,
                reply: reply_to,
                data: payload,
                writer: None,
            });
        }
    }

    pub(crate) fn commit(&mut self) {
        self.committed = self.written;
    }

    pub(crate) async fn send(&mut self, op: ClientOp<'_>) {
        if let Some(stream) = self.stream.as_mut() {
            let _ = encode(stream, op).await;
        }
    }
}

impl AsyncWrite for Writer {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let res = match self.stream.as_mut() {
            Some(stream) => {
                // Inject random I/O failures when testing.
                match inject_io_failure() {
                    Ok(()) => futures::ready!(stream.as_mut().poll_write(cx, buf)),
                    Err(err) => Err(err),
                }
            }
            None => {
                let n = buf.len();
                if self.bytes.len() - self.written < n {
                    Err(Error::new(
                        ErrorKind::Other,
                        "the disconnection buffer is full",
                    ))
                } else {
                    let range = self.written..self.written + n;
                    self.bytes[range].copy_from_slice(&buf[..n]);
                    self.written += n;
                    Ok(n)
                }
            }
        };

        // In case of I/O errors, drop the connection and clear the pong list.
        if res.is_err() {
            self.stream = None;
            self.pongs.clear();
        }
        Poll::Ready(res)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let res = match self.stream.as_mut() {
            Some(stream) => {
                // Inject random I/O failures when testing.
                match inject_io_failure() {
                    Ok(()) => futures::ready!(stream.as_mut().poll_flush(cx)),
                    Err(err) => Err(err),
                }
            }
            None => Ok(()),
        };

        // In case of I/O errors, drop the connection and clear the pong list.
        if res.is_err() {
            self.stream = None;
            self.pongs.clear();
        }
        Poll::Ready(res)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}
