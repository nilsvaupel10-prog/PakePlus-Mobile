use crate::command::model::PreviewContext;
use crate::command::model::ServerState;
use base64::prelude::*;
use serde::Serialize;
use std::fs;
use std::fs::File;
use std::io;
#[cfg(desktop)]
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{
    path::BaseDirectory, utils::config::WindowConfig, AppHandle, Emitter, Manager,
};
use tauri_plugin_http::reqwest::Client;
use walkdir::WalkDir;
use warp::Filter;
use zip::write::FileOptions;
use zip::ZipArchive;
use zip::ZipWriter;

#[cfg(desktop)]
use notify_rust::Notification;
#[cfg(desktop)]
use std::env;
#[cfg(desktop)]
use std::process::Command;
#[cfg(desktop)]
use std::time::Instant;
#[cfg(desktop)]
use tauri::WindowEvent;
#[cfg(desktop)]
use tauri::LogicalSize;
#[cfg(mobile)]
use tauri_plugin_notification::{NotificationExt, PermissionState};
#[cfg(mobile)]
use tauri_plugin_opener::OpenerExt;
#[cfg(mobile)]
use tauri::WebviewUrl;

/// Route that serves the entry HTML of the preview with the custom JavaScript
/// injected into it. It is used by the Android preview, where the preview is
/// rendered by the system browser instead of a second webview window.
const PREVIEW_ROUTE: &str = "__pakeplus_preview.html";

#[tauri::command]
pub async fn start_server(
    app: AppHandle,
    state: tauri::State<'_, Arc<Mutex<ServerState>>>,
    path: String,
    port: u16,
) -> Result<u16, String> {
    let mut state = state.lock().unwrap();
    // if server is running, stop it
    if let Some(handle) = state.server_handle.take() {
        handle.abort();
    }
    let path_clone = path.clone();
    // if port is 0, find a free port
    let port = if port == 0 {
        find_port().unwrap()
    } else {
        port
    };
    // println!("port: {}", port);
    let inject_js = state.inject_js.clone();
    let server_handle = tokio::spawn(async move {
        let static_files = warp::fs::dir(path_clone.clone());

        let oauth_callback = warp::path("callback")
            .and(warp::query::<std::collections::HashMap<String, String>>())
            .map(move |params: std::collections::HashMap<String, String>| {
                // println!("OAuth params: {:?}", params);
                let _ = app.emit("callback", serde_json::json!(params));
                // return a simple page
                warp::reply::html(format!(
                    r#"
                <html>
                    <body>
                        <h2>Login Success ✅</h2>
                        <p>You can close this window.</p>
                        <script>
                            window.close();
                        </script>
                    </body>
                </html>
                "#
                ))
            });

        // Serves the entry point of the preview with the custom javascript injected.
        // The Android preview is opened with the system browser, so the script
        // cannot be injected through a webview initialization script.
        let preview_page = warp::path(PREVIEW_ROUTE)
            .and(warp::get())
            .map({
                let inject_js = inject_js.clone();
                let root = path_clone.clone();
                move || {
                    let script = inject_js.lock().unwrap().clone().unwrap_or_default();
                    let entry = Path::new(&root).join("index.html");
                    match fs::read_to_string(&entry) {
                        Ok(html) => {
                            let tag = format!("<script>{}</script>", script);
                            let html = if html.contains("</body>") {
                                html.replace("</body>", format!("{tag}</body>"))
                            } else {
                                format!("{html}{tag}")
                            };
                            warp::reply::html(html)
                        }
                        Err(e) => warp::reply::html(format!(
                            "<!doctype html><html><body><p>Failed to read the preview: {e}</p></body></html>"
                        )),
                    }
                }
            });

        // routes
        let routes = oauth_callback
            .or(preview_page)
            .or(static_files)
            .map(|reply| {
                warp::reply::with_header(
                    reply,
                    "Cache-Control",
                    "no-store, no-cache, must-revalidate, max-age=0",
                )
            })
            .map(|reply| warp::reply::with_header(reply, "Vary", "*"))
            .map(|reply| warp::reply::with_header(reply, "Surrogate-Control", "no-store"))
            .map(|reply| warp::reply::with_header(reply, "Pragma", "no-cache"))
            .map(|reply| warp::reply::with_header(reply, "Expires", "0"));
        // start server
        warp::serve(routes).run(([127, 0, 0, 1], port)).await;
    });
    state.server_handle = Some(server_handle);
    Ok(port)
}

#[tauri::command]
pub async fn stop_server(state: tauri::State<'_, Arc<Mutex<ServerState>>>) -> Result<(), String> {
    let mut state = state.lock().unwrap();
    if let Some(handle) = state.server_handle.take() {
        handle.abort();
        Ok(())
    } else {
        Err("Server is not running".into())
    }
}

/// Opens the preview of the current project.
///
/// On desktop a second window ("PreView") is created. Mobile platforms only
/// support a single webview, therefore the preview is served by the local
/// server and opened with the system browser.
#[tauri::command]
pub async fn preview_from_config(
    handle: AppHandle,
    state: tauri::State<'_, Arc<Mutex<ServerState>>>,
    resize: bool,
    config: WindowConfig,
    js_content: String,
    devbug: bool,
    icon_base64: String,
) {
    #[cfg(desktop)]
    {
        let _ = &state;
        let window_label = "PreView";
        if let Some(existing_window) = handle.get_webview_window(window_label) {
            if resize {
                let new_size = LogicalSize::new(config.width, config.height);
                match existing_window.set_size(new_size) {
                    Ok(_) => println!("Window resized to {}x{}", config.width, config.height),
                    Err(e) => eprintln!("Failed to resize window: {}", e),
                }
            } else {
                // existing_window.eval(js)
                existing_window.close().unwrap();
                let start = Instant::now();
                while handle.get_webview_window(window_label).is_some() {
                    if start.elapsed().as_secs() > 2 {
                        println!("Window close took too long. Aborting.");
                        return;
                    }
                    std::thread::yield_now();
                }
            }
        }
        let mut contents = String::new();
        // custom js
        contents += js_content.as_str();
        if !resize {
            let pre_window = tauri::WebviewWindowBuilder::from_config(&handle, &config)
                .unwrap()
                .title(config.title)
                .initialization_script_for_all_frames(contents.as_str())
                .build()
                .unwrap();
            if icon_base64.len() > 0 {
                use tauri::image::Image;
                let icon_decode =
                    BASE64_STANDARD.decode(icon_base64.replace("data:image/png;base64,", "").trim());
                let icon_bytes = icon_decode.unwrap();
                let png_image = Image::from_bytes(&icon_bytes).unwrap();
                pre_window.set_icon(png_image).unwrap();
            }
            if devbug {
                pre_window.open_devtools();
            }
            pre_window.on_window_event(move |event| {
                if let WindowEvent::Destroyed = event {
                    handle.emit("stop_server", "0").unwrap();
                }
            });
        }
    }

    #[cfg(mobile)]
    {
        if resize {
            // The preview is rendered by the system browser, there is nothing to resize.
            return;
        }
        // The custom javascript is injected by the local server into the preview page.
        {
            let mut inject_js = state.inject_js.lock().unwrap();
            *inject_js = Some(js_content.clone());
        }
        let _ = devbug;
        let _ = &icon_base64;
        // The project that is previewed is kept in the storage of the webview,
        // the system browser cannot read it.
        request_preview_context(&handle, &state).await;
        let preview_url = match preview_target(&config.url) {
            PreviewTarget::LocalServer(mut url) => {
                // The page is served by the local server, the preview route
                // injects the custom javascript into it.
                url.set_path(&format!("/{PREVIEW_ROUTE}"));
                url.set_query(None);
                url.to_string()
            }
            PreviewTarget::Remote(url) => url,
            PreviewTarget::Application(path) => {
                match serve_application_page(&handle, &state, &path) {
                    Ok(url) => url,
                    Err(e) => {
                        eprintln!("failed to serve the preview page {path}: {e}");
                        return;
                    }
                }
            }
        };
        let _ = handle.opener().open_url(preview_url, None::<&str>);
    }
}

/// The page that is opened by the preview on mobile platforms.
#[cfg(mobile)]
enum PreviewTarget {
    /// A page that is served by the local server of the project.
    LocalServer(tauri::Url),
    /// A page of a remote website, it is opened unchanged.
    Remote(String),
    /// A page that belongs to the application itself.
    Application(String),
}

/// Receives the project that is previewed from the webview of the application.
///
/// The frontend keeps the project in the storage of the webview, a page that is
/// opened by the system browser cannot read that storage. The webview sends
/// the project to the backend, the backend writes it into the pages it serves.
#[tauri::command]
pub fn preview_context(
    state: tauri::State<'_, Arc<Mutex<ServerState>>>,
    project: Option<String>,
    preview_type: Option<String>,
    once: Option<String>,
) {
    let mut state = state.lock().unwrap();
    state.preview_context = Some(PreviewContext {
        project,
        preview_type,
        once,
    });
}

/// Asks the webview of the application for the project that is previewed.
///
/// The answer arrives asynchronously through the `preview_context` command, the
/// function waits a short moment for it. The preview is opened even when the
/// answer does not arrive, the page then falls back to its default values.
#[cfg(mobile)]
async fn request_preview_context(
    handle: &AppHandle,
    state: &tauri::State<'_, Arc<Mutex<ServerState>>>,
) {
    const SCRIPT: &str = "window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke('preview_context', {project: localStorage.getItem('currentProject'), previewType: localStorage.getItem('previewType'), once: (function(){try{var p=JSON.parse(localStorage.getItem('currentProject')||'{}');return p.appid?localStorage.getItem(p.appid):null}catch(e){return null}})()}).catch(function(){})";
    let window = match handle.get_webview_window("main") {
        Some(window) => window,
        None => return,
    };
    if window.eval(SCRIPT).is_err() {
        return;
    }
    for _ in 0..40 {
        let answered = state.lock().unwrap().preview_context.is_some();
        if answered {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Writes the project that is previewed into the storage of a served page.
///
/// The page reads the project from the storage of the browser, the storage of
/// the application is not shared with it. The values are written into the page
/// before the page itself is evaluated.
#[cfg(mobile)]
fn inject_preview_context(html: &str, context: &PreviewContext) -> String {
    let project = match serde_json::to_string(&context.project.clone().unwrap_or_default()) {
        Ok(value) => value,
        Err(_) => return html.to_string(),
    };
    let preview_type = match serde_json::to_string(
        &context.preview_type.clone().unwrap_or_else(|| "desktop".to_string()),
    ) {
        Ok(value) => value,
        Err(_) => return html.to_string(),
    };
    let once = match context.once.as_deref() {
        Some("1") => "try{var p=JSON.parse(localStorage.getItem('currentProject')||'{}');if(p.appid){localStorage.setItem(p.appid,'1')}}catch(e){}",
        _ => "",
    };
    let script = format!(
        "<script>try{{localStorage.setItem('currentProject',{project});localStorage.setItem('previewType',{preview_type});{once}}}catch(e){{}}</script>"
    );
    insert_into_head(html, &script)
}

/// Inserts a script into the head of an HTML document.
#[cfg(mobile)]
fn insert_into_head(html: &str, script: &str) -> String {
    let head = match html.find("<head") {
        Some(index) => match html[index..].find('>') {
            Some(offset) => index + offset + 1,
            None => return format!("{script}{html}"),
        },
        None => 0,
    };
    let mut result = String::with_capacity(html.len() + script.len());
    result.push_str(&html[..head]);
    result.push_str(script);
    result.push_str(&html[head..]);
    result
}

/// Determines where the preview of a project has to be opened.
///
/// Android does not support a second webview window, therefore the preview is
/// rendered by the system browser:
///
/// * a page that is served by the local server of the project is opened
///   through the preview route of that server, the server injects the custom
///   javascript into the page,
/// * a page of a remote website is opened unchanged, a browser tab cannot be
///   initialised with an initialisation script,
/// * a page of the application itself is served by the local server as well,
///   the frontend assets are embedded into the binary on mobile platforms.
#[cfg(mobile)]
fn preview_target(url: &WebviewUrl) -> PreviewTarget {
    match url {
        WebviewUrl::App(path) => PreviewTarget::Application(path.to_string_lossy().to_string()),
        WebviewUrl::External(url) => match tauri::Url::parse(url.as_str()) {
            Ok(parsed) => {
                let host = parsed.host_str().unwrap_or_default();
                if host == "127.0.0.1" || host == "localhost" {
                    PreviewTarget::LocalServer(parsed)
                } else {
                    PreviewTarget::Remote(url.to_string())
                }
            }
            Err(_) => PreviewTarget::Remote(url.to_string()),
        },
        WebviewUrl::CustomProtocol(url) => PreviewTarget::Remote(url.to_string()),
    }
}

/// Serves a page of the application itself and returns the url it is served
/// under.
///
/// The frontend assets are embedded into the binary on mobile platforms, they
/// are not available as files. The asset resolver returns their content, it is
/// written into the sandbox of the application and served from there.
#[cfg(mobile)]
fn serve_application_page(
    handle: &AppHandle,
    state: &tauri::State<'_, Arc<Mutex<ServerState>>>,
    path: &str,
) -> Result<String, String> {
    let relative = path.trim_start_matches('/').trim_start_matches("./");
    if relative.is_empty() || relative.split('/').any(|part| part == "..") {
        return Err(format!("{path} is not a valid page of the application"));
    }
    let asset = handle
        .asset_resolver()
        .get(relative.to_string())
        .ok_or_else(|| format!("the application does not contain {relative}"))?;
    let mut content = String::from_utf8_lossy(&asset.bytes).to_string();
    // The page reads the project from its storage, the storage of the browser
    // is not shared with the storage of the application.
    if let Some(context) = state.lock().unwrap().preview_context.clone() {
        content = inject_preview_context(&content, &context);
    }
    let dir = crate::utils::mobile::app_data_dir(handle)?.join("preview");
    let target = dir.join(relative);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(&target, content).map_err(|e| e.to_string())?;
    let port = crate::utils::mobile::serve_preview(state, dir)?;
    Ok(format!("http://127.0.0.1:{port}/{relative}"))
}

/// Opens an url with the default application of the system.
#[tauri::command]
pub async fn open_url(app: AppHandle, url: String) {
    #[cfg(desktop)]
    {
        let _ = open::that(url);
    }
    #[cfg(mobile)]
    {
        let _ = app.opener().open_url(url, None::<&str>);
    }
}

// open devtools
#[tauri::command]
pub async fn open_devtools(handle: AppHandle) {
    #[cfg(desktop)]
    {
        let _ = handle.get_webview_window("main").unwrap().open_devtools();
    }
    #[cfg(mobile)]
    {
        // Chrome on Android does not expose the web inspector to applications.
        let _ = &handle;
        println!("devtools are not available on mobile platforms");
    }
}

#[tauri::command]
pub async fn update_init_rs(handle: AppHandle, config: String, state: bool) -> String {
    #[cfg(desktop)]
    {
        let resource_path = handle
            .path()
            .resolve("data/init.rs", BaseDirectory::Resource)
            .expect("failed to resolve resource");
        let mut main_rust = std::fs::File::open(&resource_path).unwrap();
        let mut contents = String::new();
        main_rust.read_to_string(&mut contents).unwrap();
        contents = contents.replace("WINDOWCONFIG", config.as_str());
        // 替换state
        if state {
            println!("state: true");
        } else {
            contents = contents.replace("if true {", "if false {");
        }
        // The new file content, using Base64 encoding
        let encoded_contents = BASE64_STANDARD.encode(contents);
        return encoded_contents;
    }
    #[cfg(mobile)]
    {
        // The bundle resources live inside the APK and cannot be read with the
        // file system API, therefore the desktop only helper is not available.
        let _ = (handle, config, state);
        return String::new();
    }
}

#[tauri::command]
pub async fn run_command(command: String) -> Result<String, String> {
    #[cfg(desktop)]
    {
        #[cfg(target_os = "windows")]
        let output = tokio::process::Command::new("powershell")
            .arg("-Command")
            .arg(&command)
            .creation_flags(0x08000000)
            .output()
            .await
            .map_err(|e| e.to_string())?;

        #[cfg(not(target_os = "windows"))]
        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .output()
            .await
            .map_err(|e| e.to_string())?;

        if output.status.success() {
            #[cfg(target_os = "windows")]
            {
                use encoding_rs::GBK;
                let (decoded, _, _) = GBK.decode(&output.stdout);
                Ok(decoded.into_owned())
            }
            #[cfg(not(target_os = "windows"))]
            {
                Ok(String::from_utf8_lossy(&output.stdout).to_string())
            }
        } else {
            #[cfg(target_os = "windows")]
            {
                use encoding_rs::GBK;
                let (decoded, _, _) = GBK.decode(&output.stderr);
                Err(decoded.into_owned())
            }
            #[cfg(not(target_os = "windows"))]
            {
                Err(String::from_utf8_lossy(&output.stderr).to_string())
            }
        }
    }
    #[cfg(mobile)]
    {
        let _ = command;
        Err("Running system commands is not available on mobile platforms.".into())
    }
}

/// A stable identifier of the current device.
#[tauri::command]
pub fn get_machine_uid(app: AppHandle) -> String {
    #[cfg(desktop)]
    {
        let uid: String = machine_uid::get().unwrap_or_default();
        uid
    }
    #[cfg(mobile)]
    {
        crate::utils::mobile::device_id(&app).unwrap_or_default()
    }
}

fn zip_folder(src_path: &str, dst_path: &str) -> std::io::Result<()> {
    let file = File::create(dst_path)?;
    let mut zip = ZipWriter::new(file);
    print!("src_path = {src_path}");
    print!("dst_path = {dst_path}");
    let options: FileOptions<()> =
        FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let src_path = Path::new(src_path);
    let walkdir = WalkDir::new(src_path);
    let it = walkdir.into_iter();

    for entry in it.filter_map(|e| e.ok()) {
        let path = entry.path();
        let name = path.strip_prefix(src_path).unwrap().to_str().unwrap();

        if path.is_file() {
            zip.start_file(name, options)?;
            let mut f = File::open(path)?;
            std::io::copy(&mut f, &mut zip)?;
        } else if !name.is_empty() {
            zip.add_directory(name, options)?;
        }
    }

    zip.finish()?;
    Ok(())
}

fn unzip_file(src_path: &str, dst_path: &str) -> std::io::Result<()> {
    let file = File::open(src_path)?;
    let mut archive = ZipArchive::new(file)?;
    let dst_path = Path::new(dst_path);

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let outpath = dst_path.join(file.mangled_name());

        if file.name().ends_with('/') {
            std::fs::create_dir_all(&outpath)?;
        } else {
            if let Some(p) = outpath.parent() {
                if !p.exists() {
                    std::fs::create_dir_all(p)?;
                }
            }
            let mut outfile = File::create(&outpath)?;
            std::io::copy(&mut file, &mut outfile)?;
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn compress_folder(source: String, destination: String) -> Result<(), String> {
    zip_folder(&source, &destination).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn decompress_file(source: String, destination: String) -> Result<(), String> {
    unzip_file(&source, &destination).map_err(|e| e.to_string())
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadProgress {
    file_id: String,
    downloaded: u64,
    total: u64,
}

#[tauri::command]
pub async fn download_file(
    app: AppHandle,
    url: String,
    save_path: String,
    file_id: String,
) -> Result<(), String> {
    let client = Client::new();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    // if save path is empty
    let mut save_path = save_path;
    let file_name = url.split('/').last().unwrap();
    if save_path.is_empty() {
        // On mobile platforms the download directory of the application is
        // resolved by the runtime, it is writable without any permission.
        let file_path = app
            .path()
            .resolve(file_name, BaseDirectory::Download)
            .expect("failed to resolve resource");
        save_path = file_path.to_str().unwrap().to_string();
    }
    // if file exists, add number to file name
    if Path::new(&save_path).exists() {
        let mut i = 1;
        while Path::new(&save_path).exists() {
            save_path = save_path.split('.').nth(0).unwrap().to_string()
                + &i.to_string()
                + "."
                + save_path.split('.').nth(1).unwrap();
            i += 1;
        }
    }
    let total_size = resp.content_length();
    let mut resp = resp;
    let mut file = File::create(&save_path).map_err(|e| e.to_string())?;
    let mut downloaded: u64 = 0;
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;
        app.emit(
            "download_progress",
            DownloadProgress {
                file_id: file_id.clone(),
                downloaded,
                total: total_size.unwrap_or(0),
            },
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[derive(serde::Deserialize)]
pub struct NotificationParams {
    title: String,
    body: String,
    #[cfg_attr(mobile, allow(dead_code))]
    icon: String,
}

/// Shows a notification.
///
/// The command is asynchronous on purpose: the mobile implementation of the
/// notification plugin waits for the user to answer the permission dialog, a
/// synchronous command would block the thread of the user interface.
#[tauri::command]
pub async fn notification(app: AppHandle, params: NotificationParams) -> Result<(), String> {
    #[cfg(desktop)]
    {
        let mut notifi_app = Notification::new();
        #[cfg(target_os = "macos")]
        {
            let _ = notify_rust::set_application(if tauri::is_dev() {
                "com.apple.Terminal"
            } else {
                &app.config().identifier
            });
        }
        #[cfg(windows)]
        {
            use std::path::MAIN_SEPARATOR as SEP;
            let curr_dir = get_exe_dir(app.clone(), true);
            // set the notification's System.AppUserModel.ID only when running the installed app
            if !(curr_dir.ends_with(format!("{SEP}target{SEP}debug").as_str())
                || curr_dir.ends_with(format!("{SEP}target{SEP}release").as_str()))
            {
                notifi_app.app_id(&app.config().identifier);
            }
        }
        if !params.icon.is_empty() {
            notifi_app.icon(&params.icon);
        } else {
            notifi_app.auto_icon();
        }
        tauri::async_runtime::spawn(async move {
            let _ = notifi_app
                .summary(&params.title)
                .body(&params.body)
                .show()
                .expect("show notification failed");
        });
        Ok(())
    }
    #[cfg(mobile)]
    {
        // Android 13 and above require the POST_NOTIFICATIONS runtime permission.
        match app.notification().permission_state() {
            Ok(PermissionState::Granted) => {}
            Ok(_) => {
                let _ = app.notification().request_permission();
            }
            Err(_) => {}
        }
        app.notification()
            .builder()
            .title(params.title)
            .body(params.body)
            .show()
            .map_err(|e| e.to_string())
    }
}

/// Returns the location of the application bundle.
///
/// On desktop this is the executable (or its parent directory), on mobile the
/// application is installed as a package and the data directory is used instead.
#[tauri::command]
pub fn get_exe_dir(app: AppHandle, parent: bool) -> String {
    #[cfg(desktop)]
    {
        let exe_dir = env::current_exe().unwrap();
        if parent {
            exe_dir.parent().unwrap().to_str().unwrap().to_string()
        } else {
            exe_dir.to_str().unwrap().to_string()
        }
    }
    #[cfg(mobile)]
    {
        let dir = crate::utils::mobile::app_data_dir(&app).unwrap_or_default();
        if parent {
            dir.to_str().unwrap_or_default().to_string()
        } else {
            dir.join("PakePlus")
                .to_str()
                .unwrap_or_default()
                .to_string()
        }
    }
}

// load man.json
pub fn load_man(base_dir: &str) -> Result<String, io::Error> {
    let mut man_path = PathBuf::from(base_dir);
    man_path.push("config");
    man_path.push("man");
    match fs::read_to_string(&man_path) {
        Ok(man_base64) => match BASE64_STANDARD.decode(man_base64.trim()) {
            Ok(decoded_bytes) => match String::from_utf8(decoded_bytes) {
                Ok(decoded_str) => Ok(decoded_str),
                Err(e) => Err(io::Error::new(io::ErrorKind::InvalidData, e)),
            },
            Err(e) => Err(io::Error::new(io::ErrorKind::InvalidData, e)),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e),
    }
}

// server config www dir
#[tauri::command]
pub fn get_www_dir(base_dir: &str) -> Result<String, io::Error> {
    let mut www_dir = PathBuf::from(base_dir);
    www_dir.push("config");
    www_dir.push("www");
    if fs::metadata(&www_dir).is_ok() {
        let files = fs::read_dir(&www_dir)?;
        if files.count() > 0 {
            let port = find_port().unwrap();
            let route = warp::fs::dir(www_dir);
            tokio::spawn(async move {
                warp::serve(route).run(([127, 0, 0, 1], port)).await;
            });
            return Ok(format!("http://127.0.0.1:{}", port));
        } else {
            return Ok(String::new());
        }
    }
    Ok(String::new())
}

// get config custom js
#[tauri::command]
pub fn get_config_js(base_dir: &str) -> Result<String, io::Error> {
    let mut config_dir = PathBuf::from(base_dir);
    config_dir.push("config");
    config_dir.push("inject");
    config_dir.push("custom.js");
    if fs::metadata(&config_dir).is_ok() {
        let content = fs::read_to_string(&config_dir)?;
        Ok(content)
    } else {
        Ok(String::new())
    }
}

#[tauri::command]
pub fn get_env_var(name: String) -> Result<String, String> {
    std::env::var(name).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn find_port() -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    Ok(port)
}

// copy dir all
#[tauri::command]
pub fn copy_dir(src: &Path, dst: &Path) -> Result<(), String> {
    if dst.starts_with(src) {
        return Err("Destination cannot be inside source directory".into());
    }
    if !dst.exists() {
        fs::create_dir_all(dst).expect("create dst dir failed");
    }
    for entry in fs::read_dir(src).expect("read src dir failed") {
        let entry = entry.expect("read src dir entry failed");
        let ty = entry
            .file_type()
            .expect("read src dir entry file type failed");
        if ty.is_symlink() {
            // skip symlink
            continue;
        }
        if ty.is_dir() {
            copy_dir(&entry.path(), &dst.join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), dst.join(entry.file_name())).expect("copy file failed");
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn windows_build(
    base_dir: &str,
    exe_name: &str,
    config: String,
    custom_js: String,
    html_path: String,
    script_path: String,
) -> Result<(), String> {
    #[cfg(desktop)]
    {
        let base_path = Path::new(base_dir).join(exe_name);
        if !base_path.exists() {
            fs::create_dir_all(&base_path).map_err(|e| e.to_string())?;
        }
        let config_dir = base_path.join("config").join("inject");
        if !config_dir.exists() {
            fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
        }
        let www_dir = base_path.join("config").join("www");
        if !html_path.is_empty() {
            let html_dir = Path::new(&html_path);
            if html_dir.exists() {
                copy_dir(html_dir, &www_dir).expect("copy html dir failed");
            }
        }
        let custom_js_path = config_dir.join("custom.js");
        fs::write(custom_js_path, custom_js).map_err(|e| e.to_string())?;
        let man_path = base_path.join("config").join("man");
        fs::write(man_path, config).map_err(|e| e.to_string())?;
        let www_dir = base_path.join("config").join("www");
        if !html_path.is_empty() {
            let html_dir = Path::new(&html_path);
            if html_dir.exists() {
                copy_dir(html_dir, &www_dir).expect("copy html dir failed");
            }
        }
        let exe_path = env::current_exe().unwrap();
        let exe_dir = exe_path.parent().unwrap();
        let rhexe_dir = exe_dir.join("data").join("rh.exe");
        let rh_command = format!(
            "& \"{}\" -script \"{}\"",
            rhexe_dir.to_str().unwrap(),
            script_path
        );
        run_command(rh_command).await?;
        Ok(())
    }
    #[cfg(mobile)]
    {
        let _ = (
            base_dir,
            exe_name,
            config,
            custom_js,
            html_path,
            script_path,
        );
        Err("Windows packages can only be built on desktop platforms.".into())
    }
}

#[tauri::command]
pub async fn macos_build(
    base_dir: &str,
    exe_name: &str,
    config: String,
    base64_png: String,
    custom_js: String,
    html_path: String,
) -> Result<(), String> {
    #[cfg(desktop)]
    {
        let base_path = Path::new(base_dir).join(exe_name);
        let app_dir = base_path.join("Contents");
        if !app_dir.exists() {
            fs::create_dir_all(&app_dir).expect("create app dir failed");
        }
        let config_dir = base_path.join("Contents/MacOS/config/inject");
        let resources_dir = base_path.join("Contents/Resources");
        if !config_dir.exists() {
            fs::create_dir_all(&config_dir).expect("create config dir failed");
        }
        if !resources_dir.exists() {
            fs::create_dir_all(&resources_dir).expect("create resources dir failed");
        }
        let www_dir = base_path.join("Contents/MacOS/config/www");
        if !html_path.is_empty() {
            let html_dir = Path::new(&html_path);
            if html_dir.exists() {
                copy_dir(html_dir, &www_dir).expect("copy html dir failed");
            }
        }
        let custom_js_path = config_dir.join("custom.js");
        fs::write(custom_js_path, custom_js).expect("write custom.js failed");
        let exe_path = env::current_exe().unwrap();
        let exe_dir = exe_path.parent().unwrap();
        let exe_parent_dir = exe_dir.parent().unwrap();
        let info_plist_source = exe_parent_dir.join("Info.plist");
        let info_plist_target = base_path.join("Contents/Info.plist");
        fs::copy(&info_plist_source, &info_plist_target).expect("copy Info.plist failed");
        let pp_app_target = base_path.join("Contents/MacOS/PakePlus");
        fs::copy(&exe_path, &pp_app_target).expect("copy PakePlus app failed");
        let man_path = base_path.join("Contents/MacOS/config/man");
        fs::write(man_path, config).expect("write man failed");
        if !base64_png.is_empty() {
            let _ = png_to_icns(
                base64_png.replace("data:image/png;base64,", ""),
                resources_dir.to_str().unwrap().to_string(),
            )
            .expect("convert png to icns failed");
        }
        let base_app = Path::new(base_dir).join(format!("{}.app", exe_name));
        if base_app.exists() {
            fs::remove_dir_all(&base_app).expect("delete old app failed");
        }
        fs::rename(base_path, base_app).expect("rename app failed");
        Ok(())
    }
    #[cfg(mobile)]
    {
        let _ = (base_dir, exe_name, config, base64_png, custom_js, html_path);
        Err("macOS packages can only be built on desktop platforms.".into())
    }
}

#[tauri::command]
pub async fn linux_build(
    base_dir: &str,
    exe_name: &str,
    config: String,
    base64_png: String,
    custom_js: String,
    html_path: String,
) -> Result<(), String> {
    #[cfg(desktop)]
    {
        println!("base_dir: {}", base_dir);
        println!("exe_name: {}", exe_name);
        println!("config: {}", config);
        println!("base64_png: {}", base64_png);
        println!("custom_js: {}", custom_js);
        println!("html_path: {}", html_path);
        Ok(())
    }
    #[cfg(mobile)]
    {
        let _ = (base_dir, exe_name, config, base64_png, custom_js, html_path);
        Err("Linux packages can only be built on desktop platforms.".into())
    }
}

/// Packages the current project locally.
///
/// On desktop the executable is cloned and configured. Mobile platforms cannot
/// produce an installer without the Android SDK, therefore the project is staged
/// inside the application sandbox, served by the local server and opened with
/// the system browser.
#[tauri::command]
pub async fn build_local(
    handle: AppHandle,
    state: tauri::State<'_, Arc<Mutex<ServerState>>>,
    target_dir: &str,
    project_name: &str,
    exe_name: &str,
    config: WindowConfig,
    base64_png: String,
    debug: bool,
    custom_js: String,
    html_path: String,
) -> Result<(), String> {
    #[cfg(desktop)]
    {
        let _ = &state;
        handle.emit("local-progress", "10").unwrap();
        let resource_path = handle
            .path()
            .resolve("data/man.json", BaseDirectory::Resource)
            .expect("failed to resolve resource");
        handle.emit("local-progress", "20").unwrap();
        let man_json = fs::read_to_string(&resource_path).expect("read man.json failed");
        handle.emit("local-progress", "30").unwrap();
        let mut man_json =
            serde_json::from_str::<serde_json::Value>(&man_json).expect("parse man.json failed");
        man_json["window"] = serde_json::to_value(config).unwrap();
        man_json["debug"] = serde_json::to_value(debug).unwrap();
        man_json["name"] = serde_json::to_value(project_name).unwrap();
        man_json["visible"] = serde_json::to_value(false).unwrap();
        #[cfg(target_os = "windows")]
        {
            if !base64_png.is_empty() {
                man_json["icon"] =
                    serde_json::to_value(base64_png.replace("data:image/png;base64,", "")).unwrap();
            }
        }
        let man_json_base64 = BASE64_STANDARD.encode(man_json.to_string());
        handle.emit("local-progress", "40").unwrap();
        #[cfg(target_os = "windows")]
        {
            let script_path = handle
                .path()
                .resolve("rhscript.txt", BaseDirectory::AppData)
                .expect("failed to resolve resource");
            windows_build(
                target_dir,
                exe_name,
                man_json_base64,
                custom_js,
                html_path,
                script_path.to_str().unwrap().to_string(),
            )
            .await?;
        }
        handle.emit("local-progress", "60").unwrap();
        #[cfg(target_os = "macos")]
        macos_build(
            target_dir,
            exe_name,
            man_json_base64,
            base64_png,
            custom_js,
            html_path,
        )
        .await?;
        handle.emit("local-progress", "80").unwrap();
        #[cfg(target_os = "linux")]
        linux_build(
            target_dir,
            exe_name,
            man_json_base64,
            base64_png,
            custom_js,
            html_path,
        )
        .await?;
        handle.emit("local-progress", "100").unwrap();
        Ok(())
    }
    #[cfg(mobile)]
    {
        let _ = (target_dir, exe_name, &base64_png);
        handle
            .emit("local-progress", "10")
            .map_err(|e| e.to_string())?;
        // The bundle resources are stored inside the APK on mobile platforms,
        // the template is therefore embedded into the binary.
        let man_json = include_str!("../../data/man.json");
        let mut man_json = serde_json::from_str::<serde_json::Value>(man_json)
            .map_err(|e| format!("parse man.json failed: {e}"))?;
        man_json["window"] = serde_json::to_value(config).map_err(|e| e.to_string())?;
        man_json["debug"] = serde_json::to_value(debug).map_err(|e| e.to_string())?;
        man_json["name"] = serde_json::to_value(project_name).map_err(|e| e.to_string())?;
        man_json["visible"] = serde_json::to_value(false).map_err(|e| e.to_string())?;
        let man_json_base64 = BASE64_STANDARD.encode(man_json.to_string());
        handle
            .emit("local-progress", "40")
            .map_err(|e| e.to_string())?;

        // Stage the project inside the sandbox of the application.
        let base = crate::utils::mobile::app_data_dir(&handle)?
            .join("packages")
            .join(if project_name.is_empty() {
                "app".to_string()
            } else {
                project_name.to_string()
            });
        let www_dir = base.join("www");
        let inject_dir = base.join("inject");
        fs::create_dir_all(&www_dir).map_err(|e| e.to_string())?;
        fs::create_dir_all(&inject_dir).map_err(|e| e.to_string())?;
        if !html_path.is_empty() {
            let html_dir = Path::new(&html_path);
            if html_dir.exists() {
                crate::utils::mobile::copy_dir(html_dir, &www_dir)?;
            }
        }
        fs::write(inject_dir.join("custom.js"), custom_js).map_err(|e| e.to_string())?;
        fs::write(base.join("man"), man_json_base64).map_err(|e| e.to_string())?;
        handle
            .emit("local-progress", "80")
            .map_err(|e| e.to_string())?;

        // Run the packaged application and show it in the system browser.
        let port = crate::utils::mobile::serve_dir(&state, www_dir)?;
        let url = format!("http://127.0.0.1:{port}/index.html");
        let _ = handle.opener().open_url(url, None::<&str>);
        handle
            .emit("local-progress", "100")
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[tauri::command]
pub fn png_to_icns(base64_png: String, output_dir: String) -> Result<(), String> {
    #[cfg(desktop)]
    {
        let iconset_path = format!("{}/temp.iconset", output_dir);
        if Path::new(&iconset_path).exists() {
            fs::remove_dir_all(&iconset_path)
                .map_err(|e| format!("delete old iconset dir failed: {}", e))?;
        }
        fs::create_dir_all(&iconset_path).map_err(|e| format!("create iconset dir failed: {}", e))?;
        let png_data = BASE64_STANDARD
            .decode(&base64_png)
            .map_err(|e| format!("decode base64 png failed: {}", e))?;
        let input_png_path = format!("{}/icon.png", output_dir);
        let mut png_file =
            File::create(&input_png_path).map_err(|e| format!("write png failed: {}", e))?;
        png_file
            .write_all(&png_data)
            .map_err(|e| format!("write png content failed: {}", e))?;
        let sizes = vec![16, 32, 128, 256, 512];
        for size in sizes {
            let double = size * 2;
            let filename = format!("{}/icon_{}x{}.png", iconset_path, size, size);
            let filename2x = format!("{}/icon_{}x{}@2x.png", iconset_path, size, size);
            let status1 = Command::new("sips")
                .args([
                    "-z",
                    &size.to_string(),
                    &size.to_string(),
                    &input_png_path,
                    "--out",
                    &filename,
                ])
                .status()
                .map_err(|e| format!("execute sips failed: {}", e))?;
            let status2 = Command::new("sips")
                .args([
                    "-z",
                    &double.to_string(),
                    &double.to_string(),
                    &input_png_path,
                    "--out",
                    &filename2x,
                ])
                .status()
                .map_err(|e| format!("execute sips 2x failed: {}", e))?;
            if !status1.success() || !status2.success() {
                return Err("sips convert failed".into());
            }
        }
        let icns_path = format!("{}/icon.icns", output_dir);
        let status = Command::new("iconutil")
            .args(["-c", "icns", &iconset_path, "-o", &icns_path])
            .status()
            .map_err(|e| format!("execute iconutil failed: {}", e))?;
        if !status.success() {
            return Err("iconutil convert failed".into());
        }
        let _ = fs::remove_file(&input_png_path);
        let _ = fs::remove_dir_all(&iconset_path);
        Ok(())
    }
    #[cfg(mobile)]
    {
        let _ = (base64_png, output_dir);
        Err("ICNS icons can only be created on desktop platforms.".into())
    }
}
