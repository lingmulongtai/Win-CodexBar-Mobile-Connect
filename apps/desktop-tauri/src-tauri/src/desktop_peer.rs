//! Desktop receiving is a display source, never a collector or a sharing source.
mod projection;
use codexbar::mobile_bridge::{MobileSnapshot, PeerConnections, fetch_peer_snapshot};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};
use tokio::sync::Notify;

#[derive(Default)]
pub(crate) struct PeerReceiver {
    state: Mutex<ReceiverState>,
    notify: Arc<Notify>,
}

#[derive(Default)]
struct ReceiverState {
    saved: PeerConnections,
    load_error: Option<String>,
    generation: u64,
    snapshot: Option<MobileSnapshot>,
    error: Option<String>,
    receiving: bool,
    last_received: Option<i64>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SavedPeerStatus {
    id: String,
    name: String,
    address: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PeerStatus {
    pub(crate) selected_id: Option<String>,
    pub(crate) source_id: String,
    pub(crate) name: Option<String>,
    connections: Vec<SavedPeerStatus>,
    receiving: bool,
    last_received: Option<i64>,
    connected: bool,
    error: Option<String>,
    storage_error: Option<String>,
}

#[derive(Clone)]
pub(crate) struct PeerView {
    pub(crate) name: String,
    pub(crate) source_id: String,
    pub(crate) snapshot: Option<MobileSnapshot>,
    pub(crate) error: Option<String>,
}

impl PeerView {
    pub(crate) fn providers(&self) -> Vec<crate::commands::ProviderUsageSnapshot> {
        projection::providers(self)
    }
    pub(crate) fn spend_summary(&self) -> crate::commands::UsageSpendSummary {
        projection::spend_for(self)
    }
}

fn store_path() -> Result<std::path::PathBuf, String> {
    codexbar::settings::config_store_dir()
        .map(|root| root.join("desktop-connections.json"))
        .ok_or_else(|| "Cannot locate the PC connection store".to_owned())
}

impl ReceiverState {
    fn status(&self) -> PeerStatus {
        let selected = self.saved.selected();
        let stale = self
            .last_received
            .is_none_or(|time| chrono::Utc::now().timestamp() - time > 90);
        PeerStatus {
            selected_id: self.saved.selected_id.clone(),
            source_id: selected.map_or_else(
                || "local".to_owned(),
                |c| format!("peer:{}:{}", c.id, self.generation),
            ),
            name: selected.map(|c| c.name.clone()),
            connections: self
                .saved
                .connections
                .iter()
                .map(|c| SavedPeerStatus {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    address: c.endpoint.address.to_string(),
                })
                .collect(),
            receiving: self.receiving,
            last_received: self.last_received,
            connected: selected.is_some() && !stale && self.error.is_none(),
            error: self.error.clone().or_else(|| {
                (selected.is_some() && stale && self.snapshot.is_some())
                    .then(|| "The sharing PC has not updated recently".to_owned())
            }),
            storage_error: self.load_error.clone(),
        }
    }

    fn replace(&mut self, saved: PeerConnections) {
        self.saved = saved;
        self.generation = self.generation.wrapping_add(1);
        self.snapshot = None;
        self.error = None;
        self.last_received = None;
        self.receiving = false;
    }

    fn publish(&mut self, generation: u64, result: Result<MobileSnapshot, String>) -> bool {
        if generation != self.generation || self.saved.selected().is_none() {
            return false;
        }
        self.receiving = false;
        match result {
            Ok(snapshot) => {
                self.snapshot = Some(snapshot);
                self.error = None;
                self.last_received = Some(chrono::Utc::now().timestamp());
            }
            Err(error) => self.error = Some(error),
        }
        true
    }
}

pub(crate) fn view(app: &tauri::AppHandle) -> Option<PeerView> {
    let receiver = app.try_state::<PeerReceiver>()?;
    let state = receiver.state.lock().ok()?;
    state.saved.selected()?;
    let status = state.status();
    Some(PeerView {
        name: status.name.unwrap_or_default(),
        source_id: status.source_id,
        snapshot: state.snapshot.clone(),
        error: status.error,
    })
}

pub(crate) fn display_providers(
    app: &tauri::AppHandle,
) -> Option<Vec<crate::commands::ProviderUsageSnapshot>> {
    view(app).map(|view| projection::providers(&view))
}

pub(crate) fn install(app: tauri::AppHandle) {
    let receiver = app.state::<PeerReceiver>();
    if let Ok(mut state) = receiver.state.lock() {
        match store_path().and_then(|path| {
            PeerConnections::load(&path).map_err(|_| {
                "Saved PC connections could not be read. The original file has been preserved."
                    .to_owned()
            })
        }) {
            Ok(saved) => state.replace(saved),
            Err(error) => state.load_error = Some(error),
        }
    }
    let notify = receiver.notify.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            poll(&app).await;
            tokio::select! {
                () = tokio::time::sleep(std::time::Duration::from_secs(30)) => {},
                () = notify.notified() => {},
            }
        }
    });
}

fn emit(app: &tauri::AppHandle) {
    let handle = app.clone();
    // Source changes run on the UI thread too. Resolve status there so a
    // delayed poll notification cannot publish a previously selected PC.
    let _ = app.run_on_main_thread(move || {
        let status = get_desktop_peer_status(handle.clone());
        let _ = handle.emit("desktop-peer-changed", status);
    });
    crate::tray_bridge::refresh_tray_presentation(app);
}

async fn poll(app: &tauri::AppHandle) {
    let pending = {
        let receiver = app.state::<PeerReceiver>();
        let Ok(mut state) = receiver.state.lock() else {
            return;
        };
        if state.receiving {
            return;
        }
        let Some(connection) = state.saved.selected().cloned() else {
            return;
        };
        state.receiving = true;
        (state.generation, connection.endpoint)
    };
    emit(app);
    let result = fetch_peer_snapshot(&pending.1)
        .await
        .map_err(|error| error.to_string());
    let published = app
        .state::<PeerReceiver>()
        .state
        .lock()
        .is_ok_and(|mut state| state.publish(pending.0, result));
    if published {
        emit(app);
    }
}

#[tauri::command]
pub(crate) fn get_desktop_peer_status(app: tauri::AppHandle) -> PeerStatus {
    app.state::<PeerReceiver>()
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .status()
}

fn update(
    app: &tauri::AppHandle,
    change: impl FnOnce(&mut PeerConnections) -> Result<(), String>,
) -> Result<PeerStatus, String> {
    {
        let receiver = app.state::<PeerReceiver>();
        let mut state = receiver
            .state
            .lock()
            .map_err(|_| "PC connection state is unavailable")?;
        if let Some(error) = &state.load_error {
            return Err(error.clone());
        }
        let mut saved = state.saved.clone();
        change(&mut saved)?;
        let path = store_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|_| "Could not create the PC connection store")?;
        }
        saved.save(&path).map_err(
            |_| "Could not securely save PC connections; the previous connection is unchanged",
        )?;
        state.replace(saved);
        receiver.notify.notify_one();
    }
    emit(app);
    Ok(get_desktop_peer_status(app.clone()))
}

#[tauri::command]
pub(crate) fn save_desktop_peer(
    app: tauri::AppHandle,
    name: String,
    code: String,
) -> Result<PeerStatus, String> {
    update(&app, |saved| {
        saved
            .upsert(&name, &code)
            .map(|_| ())
            .map_err(str::to_owned)
    })
}

#[tauri::command]
pub(crate) fn select_desktop_peer(
    app: tauri::AppHandle,
    id: Option<String>,
) -> Result<PeerStatus, String> {
    update(&app, |saved| saved.select(id).map_err(str::to_owned))
}

#[tauri::command]
pub(crate) fn remove_desktop_peer(app: tauri::AppHandle, id: String) -> Result<PeerStatus, String> {
    update(&app, |saved| {
        saved.remove(&id);
        Ok(())
    })
}

#[tauri::command]
pub(crate) async fn refresh_desktop_peer(app: tauri::AppHandle) {
    poll(&app).await;
}

pub(crate) use projection::{chart, history, local_usage, spend};

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> MobileSnapshot {
        MobileSnapshot {
            schema_version: 1,
            source: "win-codexbar".into(),
            generated_at_epoch_seconds: 0,
            providers: vec![],
            spend: None,
        }
    }
    fn saved() -> PeerConnections {
        let mut saved = PeerConnections::default();
        let code = codexbar::mobile_bridge::Pairing::generate()
            .code("127.0.0.1".parse().unwrap(), 43825)
            .unwrap();
        saved.upsert("Home", &code).unwrap();
        saved
    }
    #[test]
    fn delayed_result_cannot_cross_source_or_disconnect_boundary() {
        let mut state = ReceiverState::default();
        state.replace(saved());
        let generation = state.generation;
        state.replace(saved());
        assert!(!state.publish(generation, Ok(snapshot())));
        assert!(state.snapshot.is_none());
        assert!(state.publish(state.generation, Ok(snapshot())));
        state.replace(PeerConnections::default());
        assert!(!state.publish(state.generation, Ok(snapshot())));
        assert!(state.snapshot.is_none());
    }
    #[test]
    fn failure_retains_last_good_data_and_marks_it_offline_without_fallback() {
        let mut state = ReceiverState::default();
        state.replace(saved());
        assert!(state.publish(state.generation, Ok(snapshot())));
        assert!(state.status().connected);
        assert!(state.publish(state.generation, Err("Offline".into())));
        assert!(!state.status().connected);
        assert!(state.snapshot.is_some());
        let json = serde_json::to_string(&state.status()).unwrap();
        assert!(
            !json.contains(
                &state
                    .saved
                    .selected()
                    .unwrap()
                    .endpoint
                    .pairing
                    .shared_key_base64_url
            )
        );
    }
}
