use libloading::Library;
use once_cell::sync::Lazy;
use rand::{Rng, rng};
use reqwest;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::json;
use std::any::Any;
use std::collections::HashSet;
use std::error::Error as StdError;
use std::ffi::{CStr, CString};
use std::fs::{self, Metadata};
use std::io::{self, Cursor, Read, Seek, Write};
use std::os::raw::c_char;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};
use std::time::Duration;
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

fn panic_payload_to_string(payload: Box<dyn Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&'static str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "unknown panic".to_string()
    }
}

fn ffi_boundary<F>(operation: F) -> *mut DevstoreFfiMessage
where
    F: FnOnce() -> *mut DevstoreFfiMessage,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)) {
        Ok(message) => message,
        Err(payload) => message_error(format!(
            "Internal SDK panic: {}",
            panic_payload_to_string(payload)
        )),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn devstore_free_message(message: *mut DevstoreFfiMessage) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        drop_message(message);
    }));
}

static API_URL: Lazy<RwLock<String>> =
    Lazy::new(|| RwLock::new("https://xbdev.store/api/".to_string()));
static RUSTLS_PROVIDER_READY: Lazy<()> = Lazy::new(|| {
    let _ = rustls::crypto::ring::default_provider().install_default();
});

const DEVSTORE_INSTALL_TAG: &str = "devstore_install";

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

fn ensure_crypto_provider() {
    Lazy::force(&RUSTLS_PROVIDER_READY);
}

fn format_error_chain(error: &dyn StdError) -> String {
    let mut parts = vec![error.to_string()];
    let mut current = error.source();
    while let Some(source) = current {
        parts.push(source.to_string());
        current = source.source();
    }
    parts.join(" -> ")
}

#[derive(Serialize, Deserialize)]
struct NotificationCache {
    shown_ids: Vec<u32>,
}

#[derive(Clone, Debug, Deserialize)]
struct DiscordInitResponse {
    session_token: String,
    expires_in: u64,
    heartbeat_interval: u64,
    username: String,
    discord_uid: String,
}

#[derive(Clone, Debug)]
struct DiscordSessionState {
    session_token: String,
    expires_in: u64,
    heartbeat_interval: u64,
    username: String,
    discord_uid: String,
}

static DISCORD_SESSION: Lazy<Mutex<Option<DiscordSessionState>>> = Lazy::new(|| Mutex::new(None));

const DISCORD_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DISCORD_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

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

fn build_http_client() -> Result<reqwest::blocking::Client, String> {
    ensure_crypto_provider();
    reqwest::blocking::Client::builder()
        .use_rustls_tls()
        .connect_timeout(DISCORD_CONNECT_TIMEOUT)
        .timeout(DISCORD_REQUEST_TIMEOUT)
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", format_error_chain(&e)))
}

fn parse_json_response(text: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|e| format!("Failed to parse JSON response: {}", e))
}

fn post_simple_verification(
    endpoint: &str,
    fields: &[(&str, &str)],
    success_message: &str,
    notification_title: &str,
) -> *mut DevstoreFfiMessage {
    let client = match build_http_client() {
        Ok(client) => client,
        Err(error) => return message_error(error),
    };

    let response = match client
        .post(format!("{}{}", api_base_url(), endpoint))
        .form(fields)
        .send()
    {
        Ok(response) => response,
        Err(error) => return message_error(format!("Error: Network error: {}", error)),
    };

    let text = response
        .text()
        .unwrap_or_else(|_| "No response message".to_string());

    let json = match parse_json_response(&text) {
        Ok(json) => json,
        Err(_) => return message_error(format!("Error: Invalid server response: {}", text)),
    };

    match json.get("status").and_then(Value::as_str) {
        Some("success") => message_success(success_message),
        Some("error") => {
            let msg = json
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Unknown error");
            let notification_result = send_notification(
                CString::new(notification_title).unwrap().as_ptr(),
                CString::new(msg).unwrap().as_ptr(),
            );
            drop_message(notification_result);
            message_error(format!("Error: {}", msg))
        }
        _ => message_error(format!("Error: Unexpected response: {}", text)),
    }
}

fn normalize_install_token(token: &str) -> Option<String> {
    let trimmed = token.trim().to_ascii_lowercase();
    if trimmed.len() != 96 || !trimmed.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    Some(trimmed)
}

fn extract_install_token_from_manifest_content(content: &str) -> Option<String> {
    let document = roxmltree::Document::parse(content).ok()?;
    for node in document.descendants() {
        if node.tag_name().name() != DEVSTORE_INSTALL_TAG {
            continue;
        }
        if let Some(text) = node.text() {
            if let Some(token) = normalize_install_token(text) {
                return Some(token);
            }
        }
    }
    None
}

fn extract_install_token_from_archive_reader<R>(reader: R) -> Result<Option<String>, String>
where
    R: Read + Seek,
{
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|e| format!("Failed to open package archive: {}", e))?;

    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|e| format!("Failed to inspect package archive: {}", e))?;
        let file_name = file.name().to_ascii_lowercase();

        if file_name.ends_with("appxmanifest.xml") {
            let mut manifest = String::new();
            file.read_to_string(&mut manifest)
                .map_err(|e| format!("Failed to read manifest from package archive: {}", e))?;
            if let Some(token) = extract_install_token_from_manifest_content(&manifest) {
                return Ok(Some(token));
            }
            continue;
        }

        if file_name.ends_with(".appx")
            || file_name.ends_with(".msix")
            || file_name.ends_with(".appxbundle")
            || file_name.ends_with(".msixbundle")
            || file_name.ends_with(".zip")
        {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|e| format!("Failed to read nested package archive: {}", e))?;
            if let Some(token) = extract_install_token_from_archive_reader(Cursor::new(bytes))? {
                return Ok(Some(token));
            }
        }
    }

    Ok(None)
}

fn extract_install_token_from_directory(path: &Path) -> Result<Option<String>, String> {
    for entry in WalkDir::new(path) {
        let entry = entry.map_err(|e| format!("Failed to inspect package directory: {}", e))?;
        if !entry.file_type().is_file() {
            continue;
        }
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case("AppxManifest.xml")
        {
            let manifest = fs::read_to_string(entry.path())
                .map_err(|e| format!("Failed to read package manifest: {}", e))?;
            if let Some(token) = extract_install_token_from_manifest_content(&manifest) {
                return Ok(Some(token));
            }
        }
    }

    Ok(None)
}

fn extract_install_token_from_path(path: &Path) -> Result<String, String> {
    if !path.exists() {
        return Err(format!("Package path does not exist: {}", path.display()));
    }

    if path.is_dir() {
        return extract_install_token_from_directory(path)?.ok_or_else(|| {
            "No DevStore install token found in the package directory.".to_string()
        });
    }

    if path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.eq_ignore_ascii_case("AppxManifest.xml"))
        .unwrap_or(false)
    {
        let manifest = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read package manifest: {}", e))?;
        return extract_install_token_from_manifest_content(&manifest)
            .ok_or_else(|| "No DevStore install token found in the package manifest.".to_string());
    }

    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    if !matches!(
        extension.as_str(),
        "appx" | "msix" | "appxbundle" | "msixbundle" | "zip"
    ) {
        return Err("Path must point to an AppxManifest.xml file, a package root directory, or an APPX/MSIX archive.".to_string());
    }

    let file = fs::File::open(path)
        .map_err(|e| format!("Failed to open package path '{}': {}", path.display(), e))?;
    extract_install_token_from_archive_reader(file)?
        .ok_or_else(|| "No DevStore install token found in the package archive.".to_string())
}

fn request_discord_init(
    secret_code: &str,
    product_id: &str,
) -> Result<DiscordInitResponse, String> {
    let client = build_http_client()?;
    let body = json!({
        "secret_code": secret_code,
        "product_id": product_id,
    });

    let response = client
        .post(format!("{}discord/init/", api_base_url()))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .map_err(|e| format!("Discord init request failed: {}", format_error_chain(&e)))?;

    let status = response.status();
    let text = response
        .text()
        .unwrap_or_else(|_| "No response body".to_string());

    if !status.is_success() {
        if let Ok(json) = parse_json_response(&text) {
            let message = json
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Discord init failed.");
            return Err(format!("Discord init failed: {}", message));
        }
        return Err(format!("Discord init failed: {}", text));
    }

    serde_json::from_str::<DiscordInitResponse>(&text)
        .map_err(|e| format!("Failed to parse Discord init response: {}", e))
}

fn current_discord_session() -> Option<DiscordSessionState> {
    DISCORD_SESSION.lock().unwrap().clone()
}

fn store_discord_session(init_response: DiscordInitResponse) {
    *DISCORD_SESSION.lock().unwrap() = Some(DiscordSessionState {
        session_token: init_response.session_token,
        expires_in: init_response.expires_in,
        heartbeat_interval: init_response.heartbeat_interval,
        username: init_response.username,
        discord_uid: init_response.discord_uid,
    });
}

fn take_discord_session() -> Option<DiscordSessionState> {
    DISCORD_SESSION.lock().unwrap().take()
}

fn post_discord_presence_command(
    session_token: &str,
    endpoint: &str,
    body: Option<Value>,
) -> Result<String, String> {
    let client = build_http_client()?;
    let url = format!("{}{}", api_base_url(), endpoint);
    let mut request = client
        .post(url)
        .header("Authorization", format!("Bearer {}", session_token))
        .header("Content-Type", "application/json");

    if let Some(body) = body {
        request = request.body(body.to_string());
    } else {
        request = request.body("{}".to_string());
    }

    let response = request
        .send()
        .map_err(|e| format!("Discord request failed: {}", format_error_chain(&e)))?;

    let status = response.status();
    let text = response
        .text()
        .unwrap_or_else(|_| "No response body".to_string());

    let json = parse_json_response(&text)
        .map_err(|_| format!("Discord request returned invalid JSON: {}", text))?;

    if !status.is_success() {
        let message = json
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Discord request failed.");
        return Err(message.to_string());
    }

    Ok(json
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Discord request completed.")
        .to_string())
}

fn shutdown_discord_runtime() -> Result<String, String> {
    let session = match take_discord_session() {
        Some(session) => session,
        None => return Ok("No active Discord session.".to_string()),
    };

    post_discord_presence_command(&session.session_token, "discord/presence/quit/", None)
}

fn send_presence_command(details: String) -> Result<String, String> {
    let session = current_discord_session()
        .ok_or_else(|| "Discord session is not initialized.".to_string())?;
    post_discord_presence_command(
        &session.session_token,
        "discord/presence/update/",
        Some(json!({ "details": details })),
    )
}

fn send_heartbeat_command() -> Result<String, String> {
    let session = current_discord_session()
        .ok_or_else(|| "Discord session is not initialized.".to_string())?;
    post_discord_presence_command(&session.session_token, "discord/presence/heartbeat/", None)
}

// end of helper functions

// Main functions that are exposed to C

#[unsafe(no_mangle)]
pub extern "C" fn get_sdk_version() -> *mut DevstoreFfiMessage {
    ffi_boundary(|| {
        const RAW_TOML: &str = include_str!("../Cargo.toml");
        let toml: Value = toml::from_str(RAW_TOML).unwrap();
        let version = toml
            .get("package")
            .and_then(|p| p.get("version"))
            .and_then(Value::as_str)
            .unwrap_or("Unknown version");
        message_success(version.to_string())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn set_custom_url(custom_url: *const c_char) -> *mut DevstoreFfiMessage {
    ffi_boundary(|| {
        let parsed_url = match parse_c_string(custom_url, "custom_url") {
            Ok(value) => value,
            Err(err) => return err,
        };
        let normalized = normalize_url(parsed_url);
        let mut guard = API_URL.write().unwrap();
        *guard = normalized.clone();
        message_success(format!("Custom URL set to {}", normalized))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn init_sdk_for_user(
    product_id: *const c_char,
    secret_code: *const c_char,
) -> *mut DevstoreFfiMessage {
    ffi_boundary(|| {
        let product_id = match parse_c_string(product_id, "product_id") {
            Ok(value) => value,
            Err(err) => return err,
        };
        let secret_code = match parse_c_string(secret_code, "secret_code") {
            Ok(value) => value,
            Err(err) => return err,
        };

        let _ = shutdown_discord_runtime();

        let init_response = match request_discord_init(secret_code, product_id) {
            Ok(response) => response,
            Err(err) => return message_error(err),
        };

        let username = init_response.username.clone();
        let expires_in = init_response.expires_in;
        let discord_uid = init_response.discord_uid.clone();
        let heartbeat_interval = init_response.heartbeat_interval;
        store_discord_session(init_response);

        message_success(format!(
            "Discord session ready for {} (expires in {}s, heartbeat {}s, discord uid {}).",
            username, expires_in, heartbeat_interval, discord_uid
        ))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn set_presence_for_user(details: *const c_char) -> *mut DevstoreFfiMessage {
    ffi_boundary(|| {
        let details = match parse_c_string(details, "details") {
            Ok(value) => value,
            Err(err) => return err,
        };

        match send_presence_command(details.to_string()) {
            Ok(message) => message_success(message),
            Err(err) => message_error(err),
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn discord_heartbeat() -> *mut DevstoreFfiMessage {
    ffi_boundary(|| match send_heartbeat_command() {
        Ok(message) => message_success(message),
        Err(err) => message_error(err),
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn discord_quit() -> *mut DevstoreFfiMessage {
    ffi_boundary(|| match shutdown_discord_runtime() {
        Ok(message) => message_success(message),
        Err(err) => message_error(err),
    })
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

    ensure_crypto_provider();
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

    ensure_crypto_provider();
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

    ensure_crypto_provider();
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

    ensure_crypto_provider();
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
    ensure_crypto_provider();
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

    ensure_crypto_provider();
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

    ensure_crypto_provider();
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
        let mut rng = rng();
        loop {
            let suffix: String = (0..3)
                .map(|_| (b'a' + rng.random_range(0..26)) as char)
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

    post_simple_verification(
        "drm/verify-ip/",
        &[("product_id", package_id)],
        "Download verified successfully.",
        "Download Verification Failed",
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn verify_download_code(
    product_id: *const c_char,
    code: *const c_char,
) -> *mut DevstoreFfiMessage {
    let product_id = match parse_c_string(product_id, "product_id") {
        Ok(value) => value,
        Err(err) => return err,
    };
    let code = match parse_c_string(code, "code") {
        Ok(value) => value,
        Err(err) => return err,
    };

    post_simple_verification(
        "drm/activate-download-code/",
        &[("product_id", product_id), ("code", code)],
        "Download activation code accepted.",
        "Download Activation Failed",
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn verify_resigned_install_token(
    product_id: *const c_char,
    install_token: *const c_char,
) -> *mut DevstoreFfiMessage {
    let product_id = match parse_c_string(product_id, "product_id") {
        Ok(value) => value,
        Err(err) => return err,
    };
    let install_token = match parse_c_string(install_token, "install_token") {
        Ok(value) => value,
        Err(err) => return err,
    };

    post_simple_verification(
        "drm/verify-install-token/",
        &[("product_id", product_id), ("install_token", install_token)],
        "DevStore install token verified.",
        "DevStore Install Verification Failed",
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn verify_resigned_package_path(
    product_id: *const c_char,
    package_or_root_path: *const c_char,
) -> *mut DevstoreFfiMessage {
    let product_id = match parse_c_string(product_id, "product_id") {
        Ok(value) => value,
        Err(err) => return err,
    };
    let package_or_root_path = match parse_c_string(package_or_root_path, "package_or_root_path") {
        Ok(value) => value,
        Err(err) => return err,
    };

    let install_token = match extract_install_token_from_path(Path::new(package_or_root_path)) {
        Ok(token) => token,
        Err(error) => return message_error(error),
    };

    post_simple_verification(
        "drm/verify-install-token/",
        &[
            ("product_id", product_id),
            ("install_token", install_token.as_str()),
        ],
        "DevStore install token verified.",
        "DevStore Install Verification Failed",
    )
}
// end of main functions

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_manifest(token: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10"
         xmlns:uap3="http://schemas.microsoft.com/appx/manifest/uap/windows10/3"
         IgnorableNamespaces="uap3">
  <Applications>
    <Application Id="App" Executable="App.exe" EntryPoint="App.Main">
      <Extensions>
        <uap3:Extension Category="windows.appExtension">
          <uap3:AppExtension Name="xbdev.store.install" Id="devstoreinstall" PublicFolder="Public">
            <uap3:Properties>
              <devstore_install>{}</devstore_install>
            </uap3:Properties>
          </uap3:AppExtension>
        </uap3:Extension>
      </Extensions>
    </Application>
  </Applications>
</Package>"#,
            token
        )
    }

    fn test_zip(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut cursor);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, bytes) in entries {
                writer.start_file(name, options).unwrap();
                writer.write_all(bytes).unwrap();
            }
            writer.finish().unwrap();
        }
        cursor.into_inner()
    }

    fn temp_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!("{}_{}", name, stamp));
        path
    }

    #[test]
    fn normalize_url_appends_trailing_slash() {
        assert_eq!(
            normalize_url("https://xbdev.store/api"),
            "https://xbdev.store/api/"
        );
        assert_eq!(
            normalize_url("https://xbdev.store/api/"),
            "https://xbdev.store/api/"
        );
    }

    #[test]
    fn extract_install_token_from_manifest_content_works() {
        let token = "a".repeat(96);
        let manifest = test_manifest(&token);
        assert_eq!(
            extract_install_token_from_manifest_content(&manifest),
            Some(token)
        );
    }

    #[test]
    fn extract_install_token_from_direct_package_archive_works() {
        let token = "b".repeat(96);
        let package = test_zip(&[("AppxManifest.xml", test_manifest(&token).into_bytes())]);
        let extracted = extract_install_token_from_archive_reader(Cursor::new(package))
            .expect("package should parse");
        assert_eq!(extracted, Some(token));
    }

    #[test]
    fn extract_install_token_from_zip_wrapped_package_works() {
        let token = "c".repeat(96);
        let package = test_zip(&[("AppxManifest.xml", test_manifest(&token).into_bytes())]);
        let outer_zip = test_zip(&[("nested/app.msix", package)]);
        let extracted = extract_install_token_from_archive_reader(Cursor::new(outer_zip))
            .expect("nested archive should parse");
        assert_eq!(extracted, Some(token));
    }

    #[test]
    fn extract_install_token_from_directory_path_works() {
        let token = "d".repeat(96);
        let root = temp_path("devstore_sdk_manifest");
        fs::create_dir_all(&root).unwrap();
        let manifest_path = root.join("AppxManifest.xml");
        fs::write(&manifest_path, test_manifest(&token)).unwrap();

        let extracted = extract_install_token_from_path(&root).expect("directory should parse");
        assert_eq!(extracted, token);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn committed_header_contains_new_exports() {
        let header = include_str!("../include/devstore_sdk.h");
        assert!(header.contains("init_sdk_for_user"));
        assert!(header.contains("set_presence_for_user"));
        assert!(header.contains("discord_heartbeat"));
        assert!(header.contains("discord_quit"));
        assert!(header.contains("verify_download_code"));
        assert!(header.contains("verify_resigned_install_token"));
        assert!(header.contains("verify_resigned_package_path"));
    }
}
