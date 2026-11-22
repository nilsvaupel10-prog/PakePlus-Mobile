use std::sync::{Arc, Mutex};

/// The project that is currently previewed.
///
/// The frontend keeps the project in the storage of the webview. A page that
/// is opened by the system browser (Android has no second webview window)
/// cannot read that storage, therefore the webview hands the project over to
/// the backend before the preview is opened.
#[derive(Debug, Clone, Default)]
pub struct PreviewContext {
    /// The JSON of the project that is previewed.
    pub project: Option<String>,
    /// The kind of the preview, `desktop` or `phone`.
    pub preview_type: Option<String>,
    /// The flag that remembers that the password of the project was entered,
    /// it is used by the `oncePwd` start method.
    pub once: Option<String>,
    /// The number of the request the context belongs to.
    ///
    /// The webview answers asynchronously, an answer of an older request must
    /// not be used for a newer preview.
    pub request_id: u64,
}

pub struct ServerState {
    /// Handle of the local preview server (`start_server` / `stop_server`).
    pub server_handle: Option<tokio::task::JoinHandle<()>>,
    /// Handle of the server that hosts the locally packaged app (Android "local build").
    pub package_handle: Option<tokio::task::JoinHandle<()>>,
    /// Handle of the server that hosts the pages of the application itself
    /// (Android preview of a bundled page such as the password page).
    pub preview_handle: Option<tokio::task::JoinHandle<()>>,
    /// Custom JavaScript that is injected into the HTML served by the preview server.
    ///
    /// On desktop the script is injected into the preview window through the
    /// webview initialization script. Android has no second webview window, so the
    /// preview is opened with the system browser and the script is injected by the
    /// local server instead.
    pub inject_js: Arc<Mutex<Option<String>>>,
    /// The project that is currently previewed, see [`PreviewContext`].
    pub preview_context: Option<PreviewContext>,
    /// The number of the last request for the previewed project.
    ///
    /// It is increased for every preview, the answers of the webview carry the
    /// number of the request they belong to, an answer of an older request is
    /// discarded.
    pub preview_request_id: u64,
}

impl Default for ServerState {
    fn default() -> Self {
        Self {
            server_handle: None,
            package_handle: None,
            preview_handle: None,
            inject_js: Arc::new(Mutex::new(None)),
            preview_context: None,
            preview_request_id: 0,
        }
    }
}

// #[derive(serde::Serialize)]
// pub struct ProgressPayload {
//     pub file_id: String,
//     pub downloaded: u64,
//     pub total: Option<u64>,
// }
