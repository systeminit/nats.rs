use std::fmt;
use std::io::{self, Error, ErrorKind};
use std::sync::Arc;

use async_mutex::Mutex;
use blocking::block_on;

use crate::inject_delay;
use crate::new_client::writer::Writer;

/// A `Message` that has been published to a NATS `Subject`.
pub struct Message {
    /// The NATS `Subject` that this `Message` has been published to.
    pub subject: String,

    /// The optional reply `Subject` that may be used for sending
    /// responses when using the request/reply pattern.
    pub reply: Option<String>,

    /// The `Message` contents.
    pub data: Vec<u8>,

    pub(crate) writer: Option<Arc<Mutex<Writer>>>,
}

impl Message {
    /// Respond to a request message.
    pub fn respond(&mut self, msg: impl AsRef<[u8]>) -> io::Result<()> {
        let writer = match self.writer.as_ref() {
            None => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "No reply subject available",
                ))
            }
            Some(w) => w,
        };

        // Inject random delays when testing.
        inject_delay();

        block_on(async {
            writer
                .lock()
                .await
                .publish(
                    self.subject.as_ref(),
                    self.reply.as_ref().map(|s| s.as_str()),
                    msg.as_ref(),
                )
                .await
        })
    }
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        // TODO(stjepang): fill out fields
        write!(f, "Message")
    }
}
