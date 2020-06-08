use std::io::{self, Error, ErrorKind};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use async_mutex::Mutex;
use blocking::block_on;
use futures::channel::oneshot;

use crate::inject_delay;
use crate::new_client::client;
use crate::new_client::message::Message;
use crate::new_client::options::{ConnectionOptions, Options};
use crate::new_client::subscription::Subscription;
use crate::new_client::writer::Writer;

/// A NATS client connection.
pub struct Connection {
    writer: Arc<Mutex<Writer>>,

    /// Thread running the main future.
    thread: Option<JoinHandle<io::Result<()>>>,

    /// Close signal that stops the main future.
    close_signal: Option<oneshot::Sender<()>>,
}

impl Connection {
    pub(crate) fn connect_with_options(url: &str, options: Options) -> io::Result<Connection> {
        // Spawn a client thread.
        // TODO(stjepang): make this constant configurable
        let writer = Arc::new(Mutex::new(Writer::new(8 * 1024 * 1024)));
        let (thread, close_signal) = client::spawn(url, options, writer.clone());

        // Connection handle controlling the client thread.
        let conn = Connection {
            writer,
            thread: Some(thread),
            close_signal: Some(close_signal),
        };

        // Flush to send a ping and wait for the connection to establish.
        conn.flush()?;

        // All good! The connection is now ready.
        Ok(conn)
    }

    /// Connects a NATS client.
    pub fn connect(url: &str) -> io::Result<Connection> {
        ConnectionOptions::new().connect(url)
    }

    /// Publishes a message.
    pub fn publish(&self, subject: &str, msg: impl AsRef<[u8]>) -> io::Result<()> {
        // Inject random delays when testing.
        inject_delay();

        block_on(async {
            self.writer
                .lock()
                .await
                .publish(subject, None, msg.as_ref())
                .await
        })
    }

    pub fn new_inbox(&self) -> String {
        format!("_INBOX.{}", nuid::next())
    }

    /// Publish a message on the given subject with a reply subject for responses.
    pub fn request(&self, subject: &str, msg: impl AsRef<[u8]>) -> io::Result<Message> {
        // Publish a request.
        let mut sub = self.prepare_request(subject, msg)?;

        // Wait for the response.
        sub.next()
    }

    pub fn request_timeout(
        &self,
        subject: &str,
        msg: impl AsRef<[u8]>,
        timeout: Duration,
    ) -> io::Result<Option<Message>> {
        // Publish a request.
        let mut sub = self.prepare_request(subject, msg)?;
        println!("PREPARED");

        // Wait for the response.
        dbg!(sub.next_timeout(timeout))
    }

    fn prepare_request(&self, subject: &str, msg: impl AsRef<[u8]>) -> io::Result<Subscription> {
        let reply_to = self.new_inbox();
        let sub = self.subscribe(&reply_to)?;

        let payload = msg.as_ref();
        let reply_to = Some(reply_to.as_str());

        // Inject random delays when testing.
        inject_delay();

        block_on(async {
            self.writer
                .lock()
                .await
                .publish(subject, reply_to, payload)
                .await?;
            Ok(sub)
        })
    }

    /// Creates a new subscriber.
    pub fn subscribe(&self, subject: &str) -> io::Result<Subscription> {
        // Inject random delays when testing.
        inject_delay();

        let (sid, receiver) =
            block_on(async { self.writer.lock().await.subscribe(subject, None).await })?;

        self.flush()?;
        Ok(Subscription::new(sid, receiver, self.writer.clone()))
    }

    /// Flushes by performing a round trip to the server.
    pub fn flush(&self) -> io::Result<()> {
        // Inject random delays when testing.
        inject_delay();

        let pong = block_on(async { self.writer.lock().await.ping().await })?;

        // Wait until the PONG operation is received.
        match block_on(pong) {
            Ok(()) => Ok(()),
            Err(_) => Err(Error::new(ErrorKind::ConnectionReset, "flush failed")),
        }
    }

    /// Close the connection.
    pub fn close(&mut self) -> io::Result<()> {
        // Send the close signal.
        self.close_signal.take();

        if let Some(thread) = self.thread.take() {
            // Wait for the client thread to stop.
            thread
                .join()
                .expect("client thread has panicked")
                .map_err(|err| Error::new(ErrorKind::ConnectionReset, err))?;
        }

        Ok(())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // Close the connection in case it hasn't been already.
        let _ = self.close();
    }
}
