//! Application-wide state: saved connections and their live status.

use std::collections::HashMap;

use dbclaw_core::model::ConnectionConfig;
use dbclaw_drivers::Connection;
use gpui_kit::*;

use crate::util::io;

#[derive(Clone)]
pub enum LiveStatus {
    Connecting,
    Connected(Connection),
    Failed(SharedString),
}

pub enum AppEvent {
    /// The list of saved connections changed.
    ConnectionsChanged,
    /// A connection's live status changed.
    StatusChanged(String),
}

pub struct AppState {
    pub connections: Vec<ConnectionConfig>,
    pub live: HashMap<String, LiveStatus>,
}

impl EventEmitter<AppEvent> for AppState {}

impl AppState {
    pub fn new() -> Self {
        Self { connections: crate::store::load_connections(), live: HashMap::new() }
    }

    pub fn config(&self, id: &str) -> Option<&ConnectionConfig> {
        self.connections.iter().find(|c| c.id == id)
    }

    pub fn connection(&self, id: &str) -> Option<Connection> {
        match self.live.get(id) {
            Some(LiveStatus::Connected(c)) => Some(c.clone()),
            _ => None,
        }
    }

    pub fn status(&self, id: &str) -> Option<&LiveStatus> {
        self.live.get(id)
    }

    fn persist(&self, cx: &mut Context<Self>) {
        if let Err(e) = crate::store::save_connections(&self.connections) {
            eprintln!("db-claw: failed to save connections: {e:#}");
        }
        cx.emit(AppEvent::ConnectionsChanged);
        cx.notify();
    }

    /// Add or replace a saved connection. Editing a connection drops its
    /// live session so the new settings take effect on the next connect.
    pub fn upsert(&mut self, config: ConnectionConfig, cx: &mut Context<Self>) {
        let id = config.id.clone();
        if let Some(existing) = self.connections.iter_mut().find(|c| c.id == id) {
            *existing = config;
            self.live.remove(&id);
            cx.emit(AppEvent::StatusChanged(id));
        } else {
            self.connections.push(config);
        }
        self.persist(cx);
    }

    pub fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        self.connections.retain(|c| c.id != id);
        self.live.remove(id);
        self.persist(cx);
    }

    pub fn disconnect(&mut self, id: &str, cx: &mut Context<Self>) {
        self.live.remove(id);
        cx.emit(AppEvent::StatusChanged(id.to_string()));
        cx.notify();
    }

    /// Connect (if not connected or connecting already).
    pub fn connect(&mut self, id: &str, cx: &mut Context<Self>) {
        if matches!(self.live.get(id), Some(LiveStatus::Connecting | LiveStatus::Connected(_))) {
            return;
        }
        let Some(config) = self.config(id).cloned() else { return };
        let id = id.to_string();
        self.live.insert(id.clone(), LiveStatus::Connecting);
        cx.emit(AppEvent::StatusChanged(id.clone()));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = io(Connection::open(config)).await;
            this.update(cx, |this, cx| {
                // Ignore results for connections that were removed/disconnected meanwhile.
                if !matches!(this.live.get(&id), Some(LiveStatus::Connecting)) {
                    return;
                }
                let status = match result {
                    Ok(conn) => LiveStatus::Connected(conn),
                    Err(e) => LiveStatus::Failed(dbclaw_drivers::error_message(&e).into()),
                };
                this.live.insert(id.clone(), status);
                cx.emit(AppEvent::StatusChanged(id));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}
