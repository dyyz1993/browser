//! Multi-connection WebSocket manager for the JS bridge.
//!
//! WebSocket is a long-lived async connection (unlike XHR/fetch which block +
//! setTimeout). This module spawns one OS thread per connection, each running
//! its own tokio runtime + `WebSocket` client. The main (JS) thread interacts
//! via shared queues — no runtime context issues.
//!
//! - Commands (main → background): `Arc<Mutex<VecDeque<WsCmd>>>`
//!   The background thread polls this every 10ms via `tokio::select!`.
//! - Events (background → main): `Arc<Mutex<VecDeque<WsEvent>>>`
//!   The JS event loop calls `drain_events()` each tick to fire callbacks.
//!
//! `&self` methods so it composes cleanly with a thread-local `RefCell`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::client::{Message, WebSocket};

/// Event surfaced from a background connection to the main thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsEvent {
    /// Handshake completed, readyState = OPEN.
    Open { id: u32 },
    /// Text message received.
    Text { id: u32, data: String },
    /// Binary message received.
    Binary { id: u32, data: Vec<u8> },
    /// Connection closed (code 1000-4999 or None if abnormal).
    Closed {
        id: u32,
        code: Option<u16>,
        reason: String,
    },
    /// Connection failed (handshake error, recv error, etc).
    Error { id: u32, message: String },
}

/// Command queued from main thread to a background connection.
#[derive(Debug, Clone)]
enum WsCmd {
    SendText(String),
    SendBinary(Vec<u8>),
    Close,
}

/// Per-connection state held by the manager.
struct ConnState {
    cmds: Arc<Mutex<VecDeque<WsCmd>>>,
}

/// Manages multiple WebSocket connections, each on its own OS thread.
///
/// Not `Send` (uses `Cell`/`RefCell`); lives in a thread-local on the JS thread.
pub struct WsManager {
    next_id: std::cell::Cell<u32>,
    events: Arc<Mutex<VecDeque<WsEvent>>>,
    conns: std::cell::RefCell<Vec<(u32, ConnState)>>,
}

impl Default for WsManager {
    fn default() -> Self {
        Self::new()
    }
}

impl WsManager {
    pub fn new() -> Self {
        Self {
            next_id: 0.into(),
            events: Arc::new(Mutex::new(VecDeque::new())),
            conns: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// Open a new connection to `url` (ws://). Returns the connection id.
    /// The Open/Error event fires asynchronously via `drain_events`.
    #[allow(clippy::needless_pass_by_value)]
    pub fn connect(&self, url: String) -> u32 {
        let id = self.next_id.get();
        self.next_id.set(id.wrapping_add(1));

        let events = self.events.clone();
        let cmds = Arc::new(Mutex::new(VecDeque::<WsCmd>::new()));
        let cmds_for_thread = cmds.clone();

        // Detached background thread: owns its own tokio runtime.
        std::thread::Builder::new()
            .name(format!("ws-conn-{id}"))
            .spawn(move || run_connection(id, url, events, cmds_for_thread))
            .ok();

        self.conns.borrow_mut().push((id, ConnState { cmds }));
        id
    }

    /// Queue a text message to send. No-op if `id` unknown.
    pub fn send_text(&self, id: u32, text: String) {
        self.push_cmd(id, WsCmd::SendText(text));
    }

    /// Queue a binary message to send. No-op if `id` unknown.
    pub fn send_binary(&self, id: u32, data: Vec<u8>) {
        self.push_cmd(id, WsCmd::SendBinary(data));
    }

    /// Queue a graceful close (sends 1000). No-op if `id` unknown.
    pub fn close(&self, id: u32) {
        self.push_cmd(id, WsCmd::Close);
    }

    fn push_cmd(&self, id: u32, cmd: WsCmd) {
        if let Some((_, state)) = self.conns.borrow().iter().find(|(i, _)| *i == id) {
            if let Ok(mut q) = state.cmds.lock() {
                q.push_back(cmd);
            }
        }
    }

    /// Drain all pending events (Open/Text/Binary/Closed/Error).
    /// Called once per event-loop tick by the JS runtime.
    pub fn drain_events(&self) -> Vec<WsEvent> {
        match self.events.lock() {
            Ok(mut q) => q.drain(..).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Number of currently-tracked connections (for diagnostics).
    pub fn connection_count(&self) -> usize {
        self.conns.borrow().len()
    }
}

/// Background thread entry: own runtime → connect → recv loop.
fn run_connection(
    id: u32,
    url: String,
    events: Arc<Mutex<VecDeque<WsEvent>>>,
    cmds: Arc<Mutex<VecDeque<WsCmd>>>,
) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            push_event(
                &events,
                WsEvent::Error {
                    id,
                    message: format!("runtime build: {e}"),
                },
            );
            return;
        }
    };
    rt.block_on(async move {
        let mut ws = match WebSocket::connect(&url).await {
            Ok(w) => w,
            Err(e) => {
                push_event(
                    &events,
                    WsEvent::Error {
                        id,
                        message: format!("connect: {e}"),
                    },
                );
                return;
            }
        };
        push_event(&events, WsEvent::Open { id });

        // Poll the command queue at a fixed cadence while also waiting on recv.
        let mut ticker = tokio::time::interval(Duration::from_millis(10));
        // Drop the immediate (zero) tick so the first real tick is at +10ms.
        ticker.tick().await;

        loop {
            tokio::select! {
                msg = ws.recv_message() => match msg {
                    Ok(Message::Text(s)) => push_event(&events, WsEvent::Text { id, data: s }),
                    Ok(Message::Binary(b)) => push_event(&events, WsEvent::Binary { id, data: b }),
                    Ok(Message::Close(code, reason)) => {
                        push_event(&events, WsEvent::Closed { id, code, reason });
                        break;
                    }
                    // Ping auto-responded inside recv_message; surface nothing extra.
                    Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => {}
                    Err(e) => {
                        push_event(&events, WsEvent::Error { id, message: format!("recv: {e}") });
                        break;
                    }
                },
                _ = ticker.tick() => {
                    let mut should_close = false;
                    // Drain commands under a short-lived lock, THEN process them
                    // (avoids holding a std MutexGuard across .await points).
                    let pending: Vec<WsCmd> = match cmds.lock() {
                        Ok(mut queue) => queue.drain(..).collect(),
                        Err(_) => Vec::new(),
                    };
                    for cmd in pending {
                        match cmd {
                            WsCmd::SendText(s) => { let _ = ws.send_text(&s).await; }
                            WsCmd::SendBinary(b) => { let _ = ws.send_binary(&b).await; }
                            WsCmd::Close => { let _ = ws.close().await; should_close = true; }
                        }
                    }
                    if should_close {
                        break;
                    }
                }
            }
        }
    });
}

fn push_event(events: &Arc<Mutex<VecDeque<WsEvent>>>, e: WsEvent) {
    if let Ok(mut q) = events.lock() {
        q.push_back(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_manager_has_no_connections() {
        let m = WsManager::new();
        assert_eq!(m.connection_count(), 0);
        assert!(m.drain_events().is_empty());
    }

    #[test]
    fn ids_increment() {
        let m = WsManager::new();
        // connect() with an obviously-failing url still allocates an id synchronously.
        assert_eq!(m.next_id.get(), 0);
        let _ = m.connect("ws://127.0.0.1:1/nope".to_string());
        assert_eq!(m.next_id.get(), 1);
        assert_eq!(m.connection_count(), 1);
    }

    #[test]
    fn send_to_unknown_id_is_noop() {
        let m = WsManager::new();
        // Should not panic.
        m.send_text(999, "x".to_string());
        m.close(999);
    }

    #[test]
    fn drain_events_clears_queue() {
        let events = Arc::new(Mutex::new(VecDeque::new()));
        push_event(&events, WsEvent::Open { id: 0 });
        push_event(
            &events,
            WsEvent::Text {
                id: 0,
                data: "hi".into(),
            },
        );
        let m = WsManager {
            next_id: 0.into(),
            events: events.clone(),
            conns: std::cell::RefCell::new(Vec::new()),
        };
        let drained = m.drain_events();
        assert_eq!(drained.len(), 2);
        assert!(m.drain_events().is_empty());
    }

    #[test]
    fn wsevent_is_clone_and_eq() {
        let a = WsEvent::Open { id: 1 };
        let b = a.clone();
        assert_eq!(a, b);
    }
}
