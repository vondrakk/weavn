//! In-process real-time hub (docs/DESIGN.md §5): one broadcast channel per
//! workspace carries live events (new/changed messages, typing, presence) to all
//! connected clients. Presence is derived from live WebSocket connections — a user
//! is "active" while at least one socket is open. Ephemeral by design; nothing here
//! is persisted (the projection in Postgres is the durable record).

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;
use tokio::sync::broadcast;

#[derive(Clone, Serialize)]
pub struct PresenceEntry {
    pub id: String,
    pub display: String,
}

struct Room {
    tx: broadcast::Sender<String>,
    /// accountId -> (display name, open-connection count)
    present: HashMap<String, (String, u32)>,
}

fn new_room() -> Room {
    let (tx, _rx) = broadcast::channel(256);
    Room { tx, present: HashMap::new() }
}

/// Fan-out hub keyed by workspace (the JIRA base URL / site).
#[derive(Default)]
pub struct Hub {
    rooms: Mutex<HashMap<String, Room>>,
}

impl Hub {
    /// Publish a raw JSON event string to every client of a workspace.
    pub fn publish(&self, ws: &str, json: String) {
        let rooms = self.rooms.lock().unwrap();
        if let Some(r) = rooms.get(ws) {
            let _ = r.tx.send(json); // Err only means "no listeners" — fine
        }
    }

    /// Register a new connection: subscribe to the room and bump presence. Returns
    /// the receiver; a presence event is broadcast to everyone (including the joiner).
    pub fn join(&self, ws: &str, id: &str, display: &str) -> broadcast::Receiver<String> {
        let (rx, presence) = {
            let mut rooms = self.rooms.lock().unwrap();
            let room = rooms.entry(ws.to_string()).or_insert_with(new_room);
            let rx = room.tx.subscribe();
            let e = room.present.entry(id.to_string()).or_insert_with(|| (display.to_string(), 0));
            e.0 = display.to_string();
            e.1 += 1;
            (rx, presence_json(room))
        };
        self.publish(ws, presence);
        rx
    }

    /// Drop a connection: decrement presence (removing the user at zero) and
    /// broadcast the updated presence list.
    pub fn leave(&self, ws: &str, id: &str) {
        let presence = {
            let mut rooms = self.rooms.lock().unwrap();
            let Some(room) = rooms.get_mut(ws) else { return };
            if let Some(e) = room.present.get_mut(id) {
                if e.1 <= 1 {
                    room.present.remove(id);
                } else {
                    e.1 -= 1;
                }
            }
            presence_json(room)
        };
        self.publish(ws, presence);
    }
}

fn presence_json(room: &Room) -> String {
    let mut users: Vec<PresenceEntry> = room
        .present
        .iter()
        .map(|(id, (d, _))| PresenceEntry { id: id.clone(), display: d.clone() })
        .collect();
    users.sort_by(|a, b| a.display.cmp(&b.display));
    serde_json::json!({ "type": "presence", "users": users }).to_string()
}
