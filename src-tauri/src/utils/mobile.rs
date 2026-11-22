//! Android / iOS specific helpers.
//!
//! The desktop build of PakePlus relies on operating system primitives that do not
//! exist on mobile platforms (a windowing system, a shell, the executable on disk,
//! ...). Everything that is needed to keep the very same bridge commands working on
//! a phone is implemented here.

use crate::command::model::ServerState;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager};

/// Returns the directory where PakePlus keeps its own data on mobile platforms.
pub fn app_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("failed to resolve the app data directory: {e}"))?;
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create the app data directory: {e}"))?;
    Ok(dir)
}

/// Returns the cache directory of the application.
pub fn app_cache_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| format!("failed to resolve the app cache directory: {e}"))?;
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create the app cache directory: {e}"))?;
    Ok(dir)
}

/// A stable, per installation device identifier.
///
/// `machine-uid` is not available on Android, therefore a random identifier is
/// generated once and persisted inside the application data directory.
pub fn device_id(app: &AppHandle) -> Option<String> {
    let dir = app_data_dir(app).ok()?;
    let file = dir.join("device_id");
    if let Ok(existing) = fs::read_to_string(&file) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    let id = generate_id();
    let _ = fs::write(&file, &id);
    Some(id)
}

fn generate_id() -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    format!("pp-{:016x}", hasher.finish())
}

/// Serves a directory over `127.0.0.1` and returns the port it listens on.
///
/// The server is used to run the locally packaged application.
/// Only one packaged application server can be alive at a time.
pub fn serve_dir(state: &Arc<Mutex<ServerState>>, dir: PathBuf) -> Result<u16, String> {
    let handle = spawn_server(dir)?;
    let port = handle.0;
    let mut state = state.lock().unwrap();
    if let Some(previous) = state.package_handle.take() {
        previous.abort();
    }
    state.package_handle = Some(handle.1);
    Ok(port)
}

/// Serves the pages of the application itself and returns the port it listens on.
///
/// The frontend assets are embedded into the binary on mobile platforms, they
/// are not available as files. A page that belongs to the application (the
/// password page for example) is written into the sandbox of the application
/// and served from there, so that the system browser can load it.
pub fn serve_preview(state: &Arc<Mutex<ServerState>>, dir: PathBuf) -> Result<u16, String> {
    let handle = spawn_server(dir)?;
    let port = handle.0;
    let mut state = state.lock().unwrap();
    if let Some(previous) = state.preview_handle.take() {
        previous.abort();
    }
    state.preview_handle = Some(handle.1);
    Ok(port)
}

/// Starts a static file server and returns the port it listens on.
fn spawn_server(dir: PathBuf) -> Result<(u16, tokio::task::JoinHandle<()>), String> {
    let port = crate::command::cmds::find_port()?;
    let handle = tokio::spawn(async move {
        let routes = warp::fs::dir(dir);
        warp::serve(routes).run(([127, 0, 0, 1], port)).await;
    });
    Ok((port, handle))
}

/// Recursively copies `source` into `destination`.
pub fn copy_dir(source: &Path, destination: &Path) -> Result<(), String> {
    if !destination.exists() {
        fs::create_dir_all(destination)
            .map_err(|e| format!("failed to create {}: {e}", destination.display()))?;
    }
    let entries = fs::read_dir(source)
        .map_err(|e| format!("failed to read {}: {e}", source.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("failed to read a directory entry: {e}"))?;
        let file_type = entry
            .file_type()
            .map_err(|e| format!("failed to read the file type: {e}"))?;
        if file_type.is_symlink() {
            continue;
        }
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)
                .map_err(|e| format!("failed to copy {}: {e}", entry.path().display()))?;
        }
    }
    Ok(())
}
