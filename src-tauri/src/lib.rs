mod command;
use std::sync::{Arc, Mutex};
mod utils;
use command::model::ServerState;
#[cfg(desktop)]
use tauri::menu::*;

/// Entry point of the application.
///
/// The `mobile_entry_point` attribute is required on Android and iOS, it wires
/// the Rust application into the native runtime of the platform.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .manage(Arc::new(Mutex::new(ServerState::default())))
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init());

    // The application menu only exists on desktop platforms.
    #[cfg(desktop)]
    let builder = builder.menu(|handle| {
        let menu = Menu::with_items(
            handle,
            &[
                #[cfg(target_os = "macos")]
                &Submenu::with_items(
                    handle,
                    "Edit",
                    true,
                    &[
                        &PredefinedMenuItem::undo(handle, None)?,
                        &PredefinedMenuItem::redo(handle, None)?,
                        &PredefinedMenuItem::cut(handle, None)?,
                        &PredefinedMenuItem::copy(handle, None)?,
                        &PredefinedMenuItem::paste(handle, None)?,
                        &PredefinedMenuItem::select_all(handle, None)?,
                    ],
                )?,
            ],
        );
        menu
    });

    builder
        .invoke_handler(tauri::generate_handler![
            command::cmds::preview_from_config,
            command::cmds::preview_context,
            command::cmds::open_url,
            command::cmds::open_devtools,
            command::cmds::update_init_rs,
            command::cmds::start_server,
            command::cmds::stop_server,
            command::cmds::get_machine_uid,
            command::cmds::compress_folder,
            command::cmds::decompress_file,
            command::cmds::download_file,
            command::cmds::notification,
            command::cmds::run_command,
            command::cmds::get_env_var,
            command::cmds::find_port,
            command::cmds::get_exe_dir,
            command::cmds::windows_build,
            command::cmds::macos_build,
            command::cmds::linux_build,
            command::cmds::build_local,
        ])
        .setup(|app| {
            tauri::async_runtime::block_on(async move {
                let _ = utils::init::resolve_setup(app).await;
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
