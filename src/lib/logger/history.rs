use std::io::{self, Write};

use ringbuffer::{AllocRingBuffer, RingBuffer};
use tokio::sync::broadcast::{Receiver, Sender};
use tracing_subscriber::fmt::MakeWriter;

pub struct BroadcastWriterInner {
    sender: Sender<String>,
}

impl Write for BroadcastWriterInner {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let message = String::from_utf8_lossy(buf).to_string();
        let _ = self.sender.send(message);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub struct BroadcastWriter {
    sender: Sender<String>,
}
impl BroadcastWriter {
    pub fn new(sender: Sender<String>) -> Self {
        Self { sender }
    }
}

impl<'a> MakeWriter<'a> for BroadcastWriter {
    type Writer = BroadcastWriterInner;

    fn make_writer(&'a self) -> Self::Writer {
        BroadcastWriterInner {
            sender: self.sender.clone(),
        }
    }
}

pub struct History {
    pub history: AllocRingBuffer<String>,
    pub sender: Sender<String>,
}

impl Default for History {
    fn default() -> Self {
        let (sender, _receiver) = tokio::sync::broadcast::channel(100);
        Self {
            history: AllocRingBuffer::new(10 * 1024),
            sender,
        }
    }
}

impl History {
    pub fn push(&mut self, message: String) {
        self.history.push(message.clone());
        let _ = self.sender.send(message);
    }

    pub fn subscribe(&self) -> (Receiver<String>, Vec<String>) {
        let reader = self.sender.subscribe();
        (reader, self.history.to_vec())
    }
}
