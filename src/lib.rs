use libloading::Library;
use once_cell::sync::Lazy;
use rand::{Rng, thread_rng};
use reqwest;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::json;
use std::collections::HashSet;
use std::ffi::{CStr, CString};
use std::fs::{self, Metadata};
use std::io::{self, Write};
use std::os::raw::c_char;
use std::path::Path;
use std::path::PathBuf;
use std::sync::RwLock;
use walkdir::WalkDir;
use zip;

#[repr(u32)]
#[derive(Copy, Clone)]
pub enum DevstoreMessageStatus {
    Info = 0,
    Success = 1,
    Warning = 2,
    Error = 3,
}

#[repr(C)]
pub struct DevstoreFfiMessage {
    pub status: DevstoreMessageStatus,
    pub code: u32,
    pub message: *mut c_char,
}

fn sanitize_message(text: impl Into<String>) -> CString {
    let mut cleaned = text.into();
    cleaned = cleaned.replace('\0', " ");
    CString::new(cleaned)
        .unwrap_or_else(|_| CString::new("Message contained invalid bytes").unwrap())
}

fn build_message(
    status: DevstoreMessageStatus,
    code: u32,
    text: impl Into<String>,
) -> *mut DevstoreFfiMessage {
    let c_message = sanitize_message(text);
    let pointer = c_message.into_raw();
    let container = DevstoreFfiMessage {
        status,
        code,
        message: pointer,
    };
    Box::into_raw(Box::new(container))
}

fn message_success(text: impl Into<String>) -> *mut DevstoreFfiMessage {
    build_message(DevstoreMessageStatus::Success, 0, text)
}

fn message_info(text: impl Into<String>) -> *mut DevstoreFfiMessage {
    build_message(DevstoreMessageStatus::Info, 0, text)
}

fn message_warning(text: impl Into<String>) -> *mut DevstoreFfiMessage {
    build_message(DevstoreMessageStatus::Warning, 0, text)
}

fn message_error(text: impl Into<String>) -> *mut DevstoreFfiMessage {
    build_message(DevstoreMessageStatus::Error, 0, text)
}

fn message_with_code(
    status: DevstoreMessageStatus,
    code: u32,
    text: impl Into<String>,
) -> *mut DevstoreFfiMessage {
    build_message(status, code, text)
}

fn missing_param(name: &str) -> *mut DevstoreFfiMessage {
    message_error(format!("Missing {} parameter", name))
}

fn invalid_param(name: &str) -> *mut DevstoreFfiMessage {
    message_error(format!("Invalid {} parameter", name))
}

fn parse_c_string<'a>(
    value: *const c_char,
    name: &str,
) -> Result<&'a str, *mut DevstoreFfiMessage> {
    if value.is_null() {
        return Err(missing_param(name));
    }
    let cstr = unsafe { CStr::from_ptr(value) };
    match cstr.to_str() {
        Ok(s) if !s.is_empty() => Ok(s),
        _ => Err(invalid_param(name)),
    }
}

fn drop_message(ptr: *mut DevstoreFfiMessage) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        let stored = Box::from_raw(ptr);
        if !stored.message.is_null() {
            let _ = CString::from_raw(stored.message);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn devstore_free_message(message: *mut DevstoreFfiMessage) {
    drop_message(message);
}

static API_URL: Lazy<RwLock<String>> =
    Lazy::new(|| RwLock::new("https://xbdev.store/api/".to_string()));

fn normalize_url(url: &str) -> String {
    if url.ends_with('/') {
        url.to_string()
    } else {
        format!("{}/", url)
    }
}

fn api_base_url() -> String {
    API_URL.read().unwrap().clone()
}

#[derive(Serialize, Deserialize)]
struct NotificationCache {
    shown_ids: Vec<u32>,
}

// Helper functions that are internal to the library

fn is_sdl_available() -> bool {
    let candidates = if cfg!(target_os = "windows") {
        vec!["SDL2.dll"]
    } else if cfg!(target_os = "macos") {
        vec![
            "/usr/local/lib/libSDL2.dylib",
            "/opt/homebrew/lib/libSDL2.dylib",
            "libSDL2.dylib",
        ]
    } else {
        vec![
            "libSDL2.so",
            "/usr/lib/libSDL2.so",
            "/usr/lib/x86_64-linux-gnu/libSDL2.so",
        ]
    };

    candidates
        .into_iter()
        .any(|name| unsafe { Library::new(name).is_ok() })
}

fn is_sdl_initialized() -> bool {
    unsafe { sdl2::sys::SDL_WasInit(0) != 0 }
}

fn get_pref_path() -> PathBuf {
    if is_sdl_available() && is_sdl_initialized() {
        unsafe {
            let org = CString::new("xbdev").unwrap();
            let app = CString::new("devstoreSDK").unwrap();
            let c_path = sdl2::sys::SDL_GetPrefPath(org.as_ptr(), app.as_ptr());
            if !c_path.is_null() {
                let rust_str = CStr::from_ptr(c_path).to_string_lossy().into_owned();
                return PathBuf::from(rust_str);
            }
        }
    }

    // Fallback if SDL not available or not initialized
    let mut path = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("xbdev_devstoreSDK");
    match fs::create_dir_all(&path) {
        Ok(_) => path,
        Err(_) => {
            eprintln!("Error: Failed to create directory");
            path
        }
    }
}

fn get_cache_file_path() -> PathBuf {
    let mut path = get_pref_path();
    fs::create_dir_all(&path).ok();
    path.push("notification_store.json");
    path
}

fn load_notification_cache() -> HashSet<u32> {
    let path = get_cache_file_path();
    if let Ok(content) = fs::read_to_string(&path) {
        if let Ok(cache) = serde_json::from_str::<NotificationCache>(&content) {
            return cache.shown_ids.into_iter().collect();
        }
    }
    HashSet::new()
}

fn save_notification_cache(cache: &HashSet<u32>) {
    let path = get_cache_file_path();
    let store = NotificationCache {
        shown_ids: cache.iter().cloned().collect(),
    };
    if let Ok(data) = serde_json::to_string_pretty(&store) {
        let _ = fs::write(path, data);
    }
}

// end of helper functions

// Main functions that are exposed to C

#[unsafe(no_mangle)]
pub extern "C" fn get_sdk_version() -> *mut DevstoreFfiMessage {
    const RAW_TOML: &str = include_str!("../Cargo.toml");
    let toml: Value = toml::from_str(RAW_TOML).unwrap();
    let version = toml
        .get("package")
        .and_then(|p| p.get("version"))
        .and_then(Value::as_str)
        .unwrap_or("Unknown version");
    message_success(version.to_string())
}

#[unsafe(no_mangle)]
pub extern "C" fn set_custom_url(custom_url: *const c_char) -> *mut DevstoreFfiMessage {
    let parsed_url = match parse_c_string(custom_url, "custom_url") {
        Ok(value) => value,
        Err(err) => return err,
    };
    let normalized = normalize_url(parsed_url);
    let mut guard = API_URL.write().unwrap();
    *guard = normalized.clone();
    message_success(format!("Custom URL set to {}", normalized))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn upload_save_to_server(
    package_id: *const c_char,
    user_secret: *const c_char,
    file_or_folder_path: *const c_char,
) -> *mut DevstoreFfiMessage {
    let package_id = match parse_c_string(package_id, "package_id") {
        Ok(value) => value,
        Err(err) => return err,
    };
    let user_secret = match parse_c_string(user_secret, "user_secret") {
        Ok(value) => value,
        Err(err) => return err,
    };
    let file_or_folder_path = match parse_c_string(file_or_folder_path, "file_or_folder_path") {
        Ok(value) => value,
        Err(err) => return err,
    };

    let path_check: Metadata = match fs::metadata(file_or_folder_path) {
        Ok(m) => m,
        Err(_) => return message_error("Error: File or folder does not exist"),
    };

    let mut zip_data: Vec<u8> = Vec::new();
    {
        let cursor = io::Cursor::new(&mut zip_data);
        let options: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let mut zip_writer = zip::ZipWriter::new(cursor);

        if path_check.is_file() {
            println!("File found, adding to memory...");
            let file_bytes = match fs::read(file_or_folder_path) {
                Ok(b) => b,
                Err(_) => return message_error("Error: Failed to read file"),
            };
            let filename = Path::new(file_or_folder_path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("file");
            if let Err(e) = zip_writer.start_file(filename, options) {
                return message_error(format!("Error: Failed to start zip file: {}", e));
            }
            if let Err(e) = zip_writer.write_all(&file_bytes) {
                return message_error(format!("Error: Failed to write file data to zip: {}", e));
            }
        } else if path_check.is_dir() {
            println!("Folder found, zipping entire folder in memory...");
            let folder_path = Path::new(file_or_folder_path);
            for entry in WalkDir::new(folder_path) {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        return message_error(format!("Error: traversing directory: {}", e));
                    }
                };
                let path = entry.path();
                if path.is_file() {
                    let relative_path = match path.strip_prefix(folder_path) {
                        Ok(p) => p,
                        Err(e) => {
                            return message_error(format!("Error: computing relative path: {}", e));
                        }
                    };
                    let file_bytes = match fs::read(path) {
                        Ok(b) => b,
                        Err(e) => {
                            return message_error(format!(
                                "Error: Failed to read file in folder: {}",
                                e
                            ));
                        }
                    };
                    if let Err(e) = zip_writer.start_file(relative_path.to_string_lossy(), options)
                    {
                        return message_error(format!("Error: Failed to add file to zip: {}", e));
                    }
                    if let Err(e) = zip_writer.write_all(&file_bytes) {
                        return message_error(format!(
                            "Error: Failed to write file data to zip: {}",
                            e
                        ));
                    }
                }
            }
        } else {
            return message_error("Error: Path is neither a file nor a directory");
        }
        if let Err(e) = zip_writer.finish() {
            return message_error(format!("Error: Failed to finish zip archive: {}", e));
        }
    }

    let part = match reqwest::blocking::multipart::Part::bytes(zip_data)
        .file_name("XB_Save.zip")
        .mime_str("application/zip")
    {
        Ok(p) => p,
        Err(e) => {
            return message_error(format!("Error: Failed to create multipart part: {}", e));
        }
    };
    let form = reqwest::blocking::multipart::Form::new()
        .text("user_secret", user_secret.to_string())
        .text("product_id", package_id.to_string())
        .part("save_file", part);

    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(format!("{}cloud-saves/", api_base_url()))
        .multipart(form)
        .send();

    match resp {
        Ok(response) => {
            let status = response.status();
            let text = response
                .text()
                .unwrap_or_else(|_| "No response message".to_string());
            if status.is_success() {
                let parsed: Result<Value, _> = serde_json::from_str(&text);
                if let Ok(json) = parsed {
                    if let Some(msg) = json.get("message") {
                        return message_success(format!("Upload successful: {}", msg));
                    }
                }
                return message_success(format!("Upload successful: {}", text));
            } else {
                return message_error(format!("Upload failed: {}", text));
            }
        }
        Err(e) => message_error(format!("Error: {}", e)),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn download_save_from_server(
    package_id: *const c_char,
    user_secret: *const c_char,
    extract_path: *const c_char,
) -> *mut DevstoreFfiMessage {
    let package_id = match parse_c_string(package_id, "package_id") {
        Ok(value) => value,
        Err(err) => return err,
    };
    let user_secret = match parse_c_string(user_secret, "user_secret") {
        Ok(value) => value,
        Err(err) => return err,
    };
    let extract_path = match parse_c_string(extract_path, "extract_path") {
        Ok(value) => value,
        Err(err) => return err,
    };

    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(format!("{}cloud-saves/", api_base_url()))
        .query(&[("user_secret", user_secret), ("product_id", package_id)])
        .send();

    match resp {
        Ok(response) => {
            if response.status().is_success() {
                let bytes = match response.bytes() {
                    Ok(b) => b,
                    Err(e) => {
                        return message_error(format!(
                            "Error: Failed to read response bytes: {}",
                            e
                        ));
                    }
                };
                let cursor = io::Cursor::new(bytes);
                let mut zip_archive = match zip::ZipArchive::new(cursor) {
                    Ok(z) => z,
                    Err(e) => {
                        return message_error(format!("Error: Failed to open zip archive: {}", e));
                    }
                };

                for i in 0..zip_archive.len() {
                    let mut file = match zip_archive.by_index(i) {
                        Ok(f) => f,
                        Err(e) => {
                            return message_error(format!(
                                "Error: Failed to access file in zip: {}",
                                e
                            ));
                        }
                    };
                    let outpath = Path::new(extract_path).join(file.name());
                    if file.name().ends_with('/') {
                        if let Err(e) = fs::create_dir_all(&outpath) {
                            return message_error(format!(
                                "Error: Failed to create directory: {}",
                                e
                            ));
                        }
                    } else {
                        if let Some(p) = outpath.parent() {
                            if !p.exists() {
                                if let Err(e) = fs::create_dir_all(&p) {
                                    return message_error(format!(
                                        "Error: Failed to create parent directory: {}",
                                        e
                                    ));
                                }
                            }
                        }
                        let mut outfile = match fs::File::create(&outpath) {
                            Ok(f) => f,
                            Err(e) => {
                                return message_error(format!(
                                    "Error: Failed to create output file: {}",
                                    e
                                ));
                            }
                        };
                        if let Err(e) = io::copy(&mut file, &mut outfile) {
                            return message_error(format!(
                                "Error: Failed to copy file contents: {}",
                                e
                            ));
                        }
                    }
                }
                return message_success("Download and extraction successful.");
            } else {
                let text = response
                    .text()
                    .unwrap_or_else(|_| "No response message".to_string());
                return message_error(format!("Download failed: {}", text));
            }
        }
        Err(e) => message_error(format!("Error: {}", e)),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn get_version_from_id(package_id: *const c_char) -> *mut DevstoreFfiMessage {
    let package_id = match parse_c_string(package_id, "package_id") {
        Ok(value) => value,
        Err(err) => return err,
    };

    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(format!("{}version-hex/", api_base_url()))
        .query(&[("product_id", package_id)])
        .send();

    match resp {
        Ok(response) => {
            if response.status().is_success() {
                let text = response
                    .text()
                    .unwrap_or_else(|_| "No response message".to_string());
                let parsed: Result<Value, _> = serde_json::from_str(&text);
                if let Ok(json) = parsed {
                    if let Some(version) = json.get("version") {
                        return message_success(version.to_string());
                    }
                }
                return message_info(format!("Response: {}", text));
            } else {
                let text = response
                    .text()
                    .unwrap_or_else(|_| "No response message".to_string());
                return message_error(format!("Request failed: {}", text));
            }
        }
        Err(e) => message_error(format!("Request error: {}", e)),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn send_notification(
    title: *const c_char,
    body: *const c_char,
) -> *mut DevstoreFfiMessage {
    let title = match parse_c_string(title, "title") {
        Ok(value) => value,
        Err(err) => return err,
    };
    let body = match parse_c_string(body, "body") {
        Ok(value) => value,
        Err(err) => return err,
    };

    if !is_sdl_available() {
        return message_error(
            "Error: SDL2 is not available on this platform or the SDL2 library not found.",
        );
    }

    if !is_sdl_initialized() {
        match sdl2::init() {
            Ok(_) => {}
            Err(e) => return message_error(format!("Error: SDL2 init failed: {}", e)),
        };
    }

    match sdl2::messagebox::show_simple_message_box(
        sdl2::messagebox::MessageBoxFlag::INFORMATION,
        title,
        body,
        None,
    ) {
        Ok(_) => message_success(format!("Notification sent: {} - {}", title, body)),
        Err(e) => message_error(format!("Error: SDL2 messagebox failed: {}", e)),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn check_and_show_notification(
    product_id: *const c_char,
) -> *mut DevstoreFfiMessage {
    let product_id = match parse_c_string(product_id, "product_id") {
        Ok(value) => value,
        Err(err) => return err,
    };

    let client = reqwest::blocking::Client::new();
    let url = format!(
        "{}get-latest-notification-for-app/?product_id={}",
        api_base_url(),
        product_id
    );

    let resp = client.get(&url).send();

    match resp {
        Ok(resp) => {
            if resp.status().is_success() {
                let text = match resp.text() {
                    Ok(t) => t,
                    Err(e) => {
                        return message_error(format!(
                            "Error: Failed to read response text, {}",
                            e
                        ));
                    }
                };
                let json: Value = match serde_json::from_str(&text) {
                    Ok(j) => j,
                    Err(e) => return message_error(format!("Error: Failed to parse JSON, {}", e)),
                };

                let notif_id = json
                    .get("notification_id")
                    .and_then(|id| id.as_u64())
                    .unwrap_or(0) as u32;
                let title = json
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or("Notification");
                let message = json.get("message").and_then(|m| m.as_str()).unwrap_or("");

                if message.is_empty() || notif_id == 0 {
                    return message_info("No notification to show.");
                }

                let mut cache = load_notification_cache();
                if cache.contains(&notif_id) {
                    return message_info("Notification already shown.");
                }

                let c_title = CString::new(title).unwrap();
                let c_body = CString::new(message).unwrap();

                let notification_result = send_notification(c_title.as_ptr(), c_body.as_ptr());
                drop_message(notification_result);

                cache.insert(notif_id);
                save_notification_cache(&cache);

                return message_success("Notification shown.");
            } else {
                return message_info("No notification returned from server.");
            }
        }
        Err(e) => message_error(format!("HTTP request failed: {}", e)),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn init_simple_loop(product_id: *const c_char) -> *mut DevstoreFfiMessage {
    //_local_state_path: *const c_char
    // simple loop, this will be expanded to a more complex loop as the SDK grows.
    let parsed_product_id = match parse_c_string(product_id, "product_id") {
        Ok(value) => value,
        Err(err) => return err,
    };

    let id = parsed_product_id.to_owned();

    std::thread::spawn(move || {
        loop {
            let c_id = CString::new(id.clone()).unwrap();
            let message = check_and_show_notification(c_id.as_ptr());
            drop_message(message);
            std::thread::sleep(std::time::Duration::from_secs(140));
        }
    });

    message_success("Background notification loop started.")
}

#[unsafe(no_mangle)]
pub extern "C" fn is_devstore_online() -> *mut DevstoreFfiMessage {
    let client = reqwest::blocking::Client::new();
    let req = client.get(format!("{}status-check", api_base_url())).send();
    match req {
        Ok(response) => {
            let status = response.status();
            let code = status.as_u16() as u32;
            match status.as_u16() {
                200 => {
                    message_with_code(DevstoreMessageStatus::Success, code, "Devstore is online.")
                }
                503 => message_with_code(
                    DevstoreMessageStatus::Warning,
                    code,
                    "Devstore is under maintenance.",
                ),
                other => message_with_code(
                    DevstoreMessageStatus::Warning,
                    other as u32,
                    format!("Devstore returned status {}", other),
                ),
            }
        }
        Err(e) => message_error(format!("Network error: {}", e)),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn get_current_username(user_secret: *const c_char) -> *mut DevstoreFfiMessage {
    let user_secret = match parse_c_string(user_secret, "user_secret") {
        Ok(value) => value,
        Err(err) => return err,
    };

    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(format!("{}get-username-by-secret/", api_base_url()))
        .form(&[("user_secret", user_secret)])
        .send();

    match resp {
        Ok(response) => {
            let status = response.status();
            let text = response
                .text()
                .unwrap_or_else(|_| "No response message".to_string());

            if !status.is_success() {
                return message_error(format!(
                    "Error: Request failed (status {}): {}",
                    status.as_u16(),
                    text
                ));
            }

            let json: Value = match serde_json::from_str(&text) {
                Ok(j) => j,
                Err(e) => {
                    return message_error(format!("Error: Failed to parse response JSON: {}", e));
                }
            };

            match json.get("status").and_then(Value::as_str) {
                Some("success") => match json.get("username").and_then(Value::as_str) {
                    Some(username) => message_success(username.to_string()),
                    None => message_error("Error: Username missing in response"),
                },
                Some("error") => {
                    let msg = json
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Unknown error");
                    message_error(format!("Error: Server error: {}", msg))
                }
                Some(other) => {
                    message_error(format!("Error: Unexpected status in response: {}", other))
                }
                None => message_error("Error: Missing status in response"),
            }
        }
        Err(e) => message_error(format!("Error: Network error: {}", e)),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn download_update_for_product(
    package_id: *const c_char,
) -> *mut DevstoreFfiMessage {
    let package_id = match parse_c_string(package_id, "package_id") {
        Ok(value) => value,
        Err(err) => return err,
    };

    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(format!("{}get_latest_patch/", api_base_url()))
        .form(&[("product_id", package_id)])
        .send();

    let response = match resp {
        Ok(r) => r,
        Err(e) => {
            return message_error(format!("Error: Network error: {}", e));
        }
    };

    if !response.status().is_success() {
        let txt = response
            .text()
            .unwrap_or_else(|_| "No response message".to_string());
        return message_error(format!("Error: Request failed: {}", txt));
    }

    let bytes = match response.bytes() {
        Ok(b) => b,
        Err(e) => return message_error(format!("Error: Failed to read response bytes: {}", e)),
    };

    let pref_dir = get_pref_path();
    let base_update = pref_dir.join("update");
    let update_path = if base_update.exists() {
        let mut rng = thread_rng();
        loop {
            let suffix: String = (0..3)
                .map(|_| ((b'a' + rng.gen_range(0..26)) as char))
                .collect();
            let candidate = pref_dir.join(format!("update_{}", suffix));
            if !candidate.exists() {
                break candidate;
            }
        }
    } else {
        base_update
    };
    if let Err(e) = fs::create_dir_all(&update_path) {
        return message_error(format!("Error: Failed to create update dir: {}", e));
    }

    let cursor = io::Cursor::new(bytes);
    let mut zip_archive = match zip::ZipArchive::new(cursor) {
        Ok(z) => z,
        Err(e) => return message_error(format!("Error: Failed to open zip archive: {}", e)),
    };

    for i in 0..zip_archive.len() {
        let mut file = match zip_archive.by_index(i) {
            Ok(f) => f,
            Err(e) => {
                return message_error(format!("Error: Failed to access file in zip: {}", e));
            }
        };
        let outpath = update_path.join(Path::new(file.name()));
        if file.name().ends_with('/') {
            if let Err(e) = fs::create_dir_all(&outpath) {
                return message_error(format!("Error: Failed to create directory: {}", e));
            }
        } else {
            if let Some(p) = outpath.parent() {
                if !p.exists() && fs::create_dir_all(p).is_err() {
                    return message_error("Error: Failed to create parent directory");
                }
            }
            let mut outfile = match fs::File::create(&outpath) {
                Ok(f) => f,
                Err(e) => return message_error(format!("Error: Failed to create file: {}", e)),
            };
            if io::copy(&mut file, &mut outfile).is_err() {
                return message_error("Error: Failed to write file contents");
            }
        }
    }

    let curr_file = pref_dir.join("current_version.json");
    if let Ok(data) =
        serde_json::to_string_pretty(&json!({ "path": update_path.to_string_lossy().to_string() }))
    {
        let _ = fs::write(curr_file, data);
    }

    message_success("Update downloaded and extracted successfully.")
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn verify_download_v2(package_id: *const c_char) -> *mut DevstoreFfiMessage {
    let package_id = match parse_c_string(package_id, "package_id") {
        Ok(value) => value,
        Err(err) => return err,
    };

    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(format!("{}verify-download/", api_base_url()))
        .form(&[("product_id", package_id)])
        .send();

    let response = match resp {
        Ok(r) => r,
        Err(e) => {
            return message_error(format!("Error: Network error: {}", e));
        }
    };
    let txt = response
        .text()
        .unwrap_or_else(|_| "No response message".to_string());

    let json: Value = match serde_json::from_str(&txt) {
        Ok(j) => j,
        Err(_) => {
            return message_error(format!("Error: Invalid server response: {}", txt));
        }
    };

    match json.get("status").and_then(Value::as_str) {
        Some("success") => message_success("Download verified successfully."),
        Some("error") => {
            let msg = json
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Unknown error");
            let notification_result = send_notification(
                CString::new("Download Verification Failed")
                    .unwrap()
                    .as_ptr(),
                CString::new(msg).unwrap().as_ptr(),
            );
            drop_message(notification_result);
            message_error(format!("Error: {}", msg))
        }
        _ => message_error(format!("Error: Unexpected response: {}", txt)),
    }
}
// end of main functions
