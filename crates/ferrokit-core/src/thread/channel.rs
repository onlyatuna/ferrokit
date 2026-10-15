// SPDX-License-Identifier: LGPL-3.0-only
use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::object::EventSender;
use crate::signal::Signal;

/// Error returned when sending a value fails because the channel is closed.
#[derive(Debug, PartialEq, Eq)]
pub struct SendError<T>(pub T);

impl<T> fmt::Display for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sending on a closed channel")
    }
}

impl<T: fmt::Debug> std::error::Error for SendError<T> {}

/// Error returned when receiving fails because the channel is empty and closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecvError;

impl fmt::Display for RecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "receiving on an empty and closed channel")
    }
}

impl std::error::Error for RecvError {}

/// Error returned when a non-blocking receive fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TryRecvError {
    /// Channel is currently empty.
    Empty,
    /// Channel is closed and empty.
    Disconnected,
}

impl fmt::Display for TryRecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "channel is empty"),
            Self::Disconnected => write!(f, "channel is disconnected"),
        }
    }
}

impl std::error::Error for TryRecvError {}

/// Error returned when timed receive fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvTimeoutError {
    /// Timed out waiting for data.
    Timeout,
    /// Channel is closed and empty.
    Disconnected,
}

impl fmt::Display for RecvTimeoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => write!(f, "timed out receiving from channel"),
            Self::Disconnected => write!(f, "channel disconnected"),
        }
    }
}

impl std::error::Error for RecvTimeoutError {}

type ChannelSignalEmitter<T> = Arc<dyn Fn(&T) + Send + Sync>;

struct ChannelShared<T> {
    queue: Mutex<VecDeque<T>>,
    capacity: Option<usize>,
    is_closed: Mutex<bool>,
    send_cond: Condvar,
    recv_cond: Condvar,
    event_senders: Mutex<Vec<EventSender>>,
    signal_emitters: Mutex<Vec<ChannelSignalEmitter<T>>>,
}

/// Creates an unbounded channel with Qt event loop integration.
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(ChannelShared {
        queue: Mutex::new(VecDeque::new()),
        capacity: None,
        is_closed: Mutex::new(false),
        send_cond: Condvar::new(),
        recv_cond: Condvar::new(),
        event_senders: Mutex::new(Vec::new()),
        signal_emitters: Mutex::new(Vec::new()),
    });

    (
        Sender {
            shared: Arc::clone(&shared),
        },
        Receiver { shared },
    )
}

/// Creates a bounded channel with maximum capacity.
pub fn bounded<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(ChannelShared {
        queue: Mutex::new(VecDeque::new()),
        capacity: Some(capacity.max(1)),
        is_closed: Mutex::new(false),
        send_cond: Condvar::new(),
        recv_cond: Condvar::new(),
        event_senders: Mutex::new(Vec::new()),
        signal_emitters: Mutex::new(Vec::new()),
    });

    (
        Sender {
            shared: Arc::clone(&shared),
        },
        Receiver { shared },
    )
}

/// Sending end of a channel.
pub struct Sender<T> {
    shared: Arc<ChannelShared<T>>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T> Sender<T> {
    /// Sends a value into the channel. Blocks if channel is bounded and full.
    pub fn send(&self, val: T) -> Result<(), SendError<T>> {
        {
            let mut queue = self.shared.queue.lock().unwrap();
            let is_closed = *self.shared.is_closed.lock().unwrap();
            if is_closed {
                return Err(SendError(val));
            }

            if let Some(cap) = self.shared.capacity {
                while queue.len() >= cap {
                    queue = self.shared.send_cond.wait(queue).unwrap();
                    let closed = *self.shared.is_closed.lock().unwrap();
                    if closed {
                        return Err(SendError(val));
                    }
                }
            }

            queue.push_back(val);
        }

        self.shared.recv_cond.notify_one();

        // Wake up connected Qt event loops
        let event_senders = self.shared.event_senders.lock().unwrap().clone();
        for sender in event_senders {
            (sender.wakeup)();
        }

        // Notify attached signals if any
        let signal_emitters = self.shared.signal_emitters.lock().unwrap().clone();
        if !signal_emitters.is_empty() {
            let queue = self.shared.queue.lock().unwrap();
            if let Some(last) = queue.back() {
                for emitter in signal_emitters {
                    emitter(last);
                }
            }
        }

        Ok(())
    }

    /// Whether the channel has been closed.
    pub fn is_closed(&self) -> bool {
        *self.shared.is_closed.lock().unwrap()
    }
}

/// Receiving end of a channel.
pub struct Receiver<T> {
    shared: Arc<ChannelShared<T>>,
}

impl<T> Receiver<T> {
    /// Blocks until an item is available, returning an error if disconnected.
    pub fn recv(&self) -> Result<T, RecvError> {
        let mut queue = self.shared.queue.lock().unwrap();
        loop {
            if let Some(val) = queue.pop_front() {
                self.shared.send_cond.notify_one();
                return Ok(val);
            }
            if *self.shared.is_closed.lock().unwrap() {
                return Err(RecvError);
            }
            queue = self.shared.recv_cond.wait(queue).unwrap();
        }
    }

    /// Attempts to receive an item without blocking.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        let mut queue = self.shared.queue.lock().unwrap();
        if let Some(val) = queue.pop_front() {
            self.shared.send_cond.notify_one();
            Ok(val)
        } else if *self.shared.is_closed.lock().unwrap() {
            Err(TryRecvError::Disconnected)
        } else {
            Err(TryRecvError::Empty)
        }
    }

    /// Blocks until an item is available or timeout expires.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        let deadline = Instant::now() + timeout;
        let mut queue = self.shared.queue.lock().unwrap();

        loop {
            if let Some(val) = queue.pop_front() {
                self.shared.send_cond.notify_one();
                return Ok(val);
            }
            if *self.shared.is_closed.lock().unwrap() {
                return Err(RecvTimeoutError::Disconnected);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(RecvTimeoutError::Timeout);
            }
            let remaining = deadline - now;
            let (next_queue, timeout_res) = self
                .shared
                .recv_cond
                .wait_timeout(queue, remaining)
                .unwrap();
            queue = next_queue;
            if timeout_res.timed_out() && queue.is_empty() {
                return Err(RecvTimeoutError::Timeout);
            }
        }
    }

    /// Attaches an [`EventSender`] to wake up a Qt thread event loop when items are sent.
    pub fn attach_event_sender(&self, sender: EventSender) {
        self.shared.event_senders.lock().unwrap().push(sender);
    }

    /// Attaches a [`Signal`] that will be emitted whenever a new item is sent through the channel.
    pub fn connect_to_signal(&self, signal: &Signal<T>)
    where
        T: Clone + Send + 'static,
    {
        let sig_clone = signal.clone();
        let emitter = Arc::new(move |val: &T| {
            sig_clone.emit(val);
        });
        self.shared.signal_emitters.lock().unwrap().push(emitter);
    }

    /// Closes the channel.
    pub fn close(&self) {
        *self.shared.is_closed.lock().unwrap() = true;
        self.shared.send_cond.notify_all();
        self.shared.recv_cond.notify_all();
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.close();
    }
}
