//! Saved desktop receivers are independent from provider credentials and collectors.
use super::PeerEndpoint;
use serde::{Deserialize, Serialize};
use std::{io, path::Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeerConnection {
    pub id: String,
    pub name: String,
    pub endpoint: PeerEndpoint,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeerConnections {
    pub selected_id: Option<String>,
    pub connections: Vec<PeerConnection>,
}

impl PeerConnections {
    pub fn load(path: &Path) -> io::Result<Self> {
        let raw = match crate::secure_file::read_string(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        if raw.len() > 32 * 1024 {
            return Err(io::Error::other("Saved PC connections are too large"));
        }
        let value: Self = serde_json::from_str(&raw).map_err(io::Error::other)?;
        value.validate()?;
        Ok(value)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        self.validate()?;
        crate::secure_file::write_string(
            path,
            &serde_json::to_string(self).map_err(io::Error::other)?,
        )
    }

    pub fn selected(&self) -> Option<&PeerConnection> {
        self.connections
            .iter()
            .find(|c| Some(&c.id) == self.selected_id.as_ref())
    }

    pub fn upsert(&mut self, name: &str, code: &str) -> Result<String, &'static str> {
        let name = name.trim();
        if !valid_name(name) {
            return Err("Use a PC name of 1–64 characters without control characters");
        }
        let endpoint = PeerEndpoint::from_code(code).map_err(|_| "Invalid PC connection code")?;
        let index = self
            .connections
            .iter()
            .position(|c| c.endpoint.pairing.companion_id == endpoint.pairing.companion_id);
        if index.is_none() && self.connections.len() >= 16 {
            return Err("Remove a saved PC before adding another (maximum 16)");
        }
        let id = index.map_or_else(
            || uuid::Uuid::new_v4().to_string(),
            |i| self.connections[i].id.clone(),
        );
        let connection = PeerConnection {
            id: id.clone(),
            name: name.to_owned(),
            endpoint,
        };
        if let Some(index) = index {
            self.connections[index] = connection;
        } else {
            self.connections.push(connection);
        }
        self.selected_id = Some(id.clone());
        Ok(id)
    }

    pub fn select(&mut self, id: Option<String>) -> Result<(), &'static str> {
        if id
            .as_ref()
            .is_some_and(|id| !self.connections.iter().any(|c| &c.id == id))
        {
            return Err("Saved PC was not found");
        }
        self.selected_id = id;
        Ok(())
    }

    pub fn remove(&mut self, id: &str) {
        self.connections.retain(|c| c.id != id);
        if self.selected_id.as_deref() == Some(id) {
            self.selected_id = None;
        }
    }

    fn validate(&self) -> io::Result<()> {
        let mut ids = std::collections::HashSet::new();
        let mut companions = std::collections::HashSet::new();
        if self.connections.len() > 16
            || self.connections.iter().any(|c| {
                uuid::Uuid::parse_str(&c.id).is_err()
                    || !valid_name(&c.name)
                    || !c.endpoint.validate()
                    || !ids.insert(&c.id)
                    || !companions.insert(&c.endpoint.pairing.companion_id)
            })
            || self.selected_id.is_some() && self.selected().is_none()
        {
            return Err(io::Error::other("Invalid saved PC connections"));
        }
        Ok(())
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.trim() == name
        && name.chars().count() <= 64
        && !name.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn code() -> String {
        super::super::Pairing::generate()
            .code("127.0.0.1".parse().unwrap(), 43825)
            .unwrap()
    }

    #[test]
    fn reconnect_updates_one_pc_and_removing_selection_returns_to_local() {
        let mut store = PeerConnections::default();
        let code = code();
        let id = store.upsert("Home", &code).unwrap();
        assert_eq!(store.upsert("Renamed", &code).unwrap(), id);
        assert_eq!(store.connections.len(), 1);
        let other = store.upsert("Other", &self::code()).unwrap();
        store.remove(&id);
        assert_eq!(store.selected().unwrap().id, other);
        store.remove(&other);
        assert!(store.selected_id.is_none());
        assert!(store.select(Some(id)).is_err());
    }

    #[test]
    fn invalid_saved_connections_fail_closed_without_overwriting_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("peers.json");
        assert!(PeerConnections::load(&path).unwrap().connections.is_empty());
        std::fs::write(&path, "unreadable previous connection").unwrap();
        assert!(PeerConnections::load(&path).is_err());
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "unreadable previous connection"
        );
    }

    #[test]
    fn persisted_selection_roundtrips_and_rejects_untrusted_addresses() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("peers.json");
        let mut store = PeerConnections::default();
        let id = store.upsert("Home", &code()).unwrap();
        store.save(&path).unwrap();
        let mut restored = PeerConnections::load(&path).unwrap();
        assert_eq!(restored.selected().unwrap().id, id);
        restored.connections[0].endpoint.address = "8.8.8.8:43825".parse().unwrap();
        assert!(restored.save(&path).is_err());
        assert_eq!(
            PeerConnections::load(&path).unwrap().selected().unwrap().id,
            id
        );
        assert!(store.upsert("bad\nname", &code()).is_err());
    }
}
