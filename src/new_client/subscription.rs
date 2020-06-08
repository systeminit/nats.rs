use std::fmt;
use std::io::{self, ErrorKind};
use std::sync::Arc;
use std::time::Duration;

use async_mutex::Mutex;
use blocking::block_on;
use futures::channel::mpsc;
use futures::prelude::*;
use smol::Timer;

use crate::inject_delay;
use crate::new_client::message::Message;
use crate::new_client::writer::Writer;

/// A subscription to a subject.
pub struct Subscription {
    /// Subscription ID.
    sid: u64,

    /// MSG operations received from the server.
    messages: mpsc::UnboundedReceiver<Message>,

    writer: Arc<Mutex<Writer>>,
}

impl Subscription {
    /// Creates a subscription.
    pub(crate) fn new(
        sid: u64,
        messages: mpsc::UnboundedReceiver<Message>,
        writer: Arc<Mutex<Writer>>,
    ) -> Subscription {
        Subscription {
            sid,
            messages,
            writer,
        }
    }

    /// Blocks until the next message is received or the connection is closed.
    pub fn next(&mut self) -> io::Result<Message> {
        // Block on the next message in the channel.
        block_on(self.messages.next()).ok_or_else(|| ErrorKind::ConnectionReset.into())
    }

    /// Get the next message, or a timeout error if no messages are available for timeout.
    pub fn next_timeout(&mut self, timeout: Duration) -> io::Result<Option<Message>> {
        // Block on the next message in the channel.
        block_on(async move {
            futures::select! {
                msg = self.messages.next().fuse() => {
                    match msg {
                        Some(msg) => Ok(Some(msg)),
                        None => Err(ErrorKind::ConnectionReset.into()),
                    }
                }
                _ = Timer::after(timeout).fuse() => Ok(None),
            }
        })
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        // Inject random delays when testing.
        inject_delay();

        // Send an UNSUB operation to the server.
        block_on(async {
            let mut writer = self.writer.lock().await;
            let _ = writer.unsubscribe(self.sid).await;
        });
    }
}

impl fmt::Debug for Subscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        // TODO(stjepang): fill out fields
        write!(f, "Subscription")
    }
}
