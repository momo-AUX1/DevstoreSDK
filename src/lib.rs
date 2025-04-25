use reqwest;
use serde::{Serialize, Deserialize};
use serde_json::Value;
use std::ffi::{CStr, CString};
use std::fs::{self, Metadata};
use zip;
use std::io::{self, Write};
use std::os::raw::c_char;
use std::path::Path;
use walkdir::WalkDir;
use std::path::PathBuf;
use libloading::Library;
use std::collections::HashSet;

const URL: &str = "https://xbdev.store/api/";


#[derive(Serialize, Deserialize)]
struct NotificationCache {
    shown_ids: Vec<u32>,
}

// Helper functions that are internal to the library

fn string_to_c_char(s: String) -> *mut c_char {
    CString::new(s).unwrap().into_raw()
}

fn is_sdl_available() -> bool {
    let candidates = if cfg!(target_os = "windows") {
        vec!["SDL2.dll"]
    } else if cfg!(target_os = "macos") {
        vec!["/usr/local/lib/libSDL2.dylib", "/opt/homebrew/lib/libSDL2.dylib"]
    } else {
        vec!["libSDL2.so", "/usr/lib/libSDL2.so", "/usr/lib/x86_64-linux-gnu/libSDL2.so"]
    };

    candidates.into_iter().any(|name| {
        unsafe {Library::new(name).is_ok() }
    })
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
pub unsafe extern "C" fn upload_save_to_server(
    package_id: *const c_char,
    user_secret: *const c_char,
    file_or_folder_path: *const c_char
) -> *mut c_char {
    if package_id.is_null() {
        return string_to_c_char("Error: Missing package_id parameter".to_string());
    }
    if user_secret.is_null() {
        return string_to_c_char("Error: Missing user_secret parameter".to_string());
    }
    if file_or_folder_path.is_null() {
        return string_to_c_char("Error: Missing file_or_folder_path parameter".to_string());
    }
    
    let package_id = unsafe { match CStr::from_ptr(package_id).to_str() {
        Ok(s) if !s.is_empty() => s,
        _ => return string_to_c_char("Error: Invalid package_id parameter".to_string()),
    } };
    let user_secret =unsafe { match CStr::from_ptr(user_secret).to_str() {
        Ok(s) if !s.is_empty() => s,
        _ => return string_to_c_char("Error: Invalid user_secret parameter".to_string()),
    } };
    let file_or_folder_path = unsafe { match CStr::from_ptr(file_or_folder_path).to_str() {
        Ok(s) if !s.is_empty() => s,
        _ => return string_to_c_char("Error: Invalid file_or_folder_path parameter".to_string()),
    } };

    let path_check: Metadata = match fs::metadata(file_or_folder_path) {
        Ok(m) => m,
        Err(_) => return string_to_c_char("Error: File or folder does not exist".to_string()),
    };

    let mut zip_data: Vec<u8> = Vec::new();
    {
        let cursor = io::Cursor::new(&mut zip_data);
        let options: zip::write::FileOptions<()> = zip::write::FileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let mut zip_writer = zip::ZipWriter::new(cursor);
        
        if path_check.is_file() {
            println!("File found, adding to memory...");
            let file_bytes = match fs::read(file_or_folder_path) {
                Ok(b) => b,
                Err(_) => return string_to_c_char("Error: Failed to read file".to_string()),
            };
            let filename = Path::new(file_or_folder_path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("file");
            if let Err(e) = zip_writer.start_file(filename, options) {
                return string_to_c_char(format!("Error: Failed to start zip file: {}", e));
            }
            if let Err(e) = zip_writer.write_all(&file_bytes) {
                return string_to_c_char(format!("Error: Failed to write file data to zip: {}", e));
            }
        } else if path_check.is_dir() {
            println!("Folder found, zipping entire folder in memory...");
            let folder_path = Path::new(file_or_folder_path);
            for entry in WalkDir::new(folder_path) {
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => return string_to_c_char(format!("Error traversing directory: {}", e)),
                };
                let path = entry.path();
                if path.is_file() {
                    let relative_path = match path.strip_prefix(folder_path) {
                        Ok(p) => p,
                        Err(e) => return string_to_c_char(format!("Error computing relative path: {}", e)),
                    };
                    let file_bytes = match fs::read(path) {
                        Ok(b) => b,
                        Err(e) => return string_to_c_char(format!("Error: Failed to read file in folder: {}", e)),
                    };
                    if let Err(e) = zip_writer.start_file(relative_path.to_string_lossy(), options) {
                        return string_to_c_char(format!("Error: Failed to add file to zip: {}", e));
                    }
                    if let Err(e) = zip_writer.write_all(&file_bytes) {
                        return string_to_c_char(format!("Error: Failed to write file data to zip: {}", e));
                    }
                }
            }
        } else {
            return string_to_c_char("Error: Path is neither a file nor a directory".to_string());
        }
        if let Err(e) = zip_writer.finish() {
            return string_to_c_char(format!("Error: Failed to finish zip archive: {}", e));
        }
    }
    
    let part = match reqwest::blocking::multipart::Part::bytes(zip_data)
        .file_name("XB_Save.zip")
        .mime_str("application/zip") {
            Ok(p) => p,
            Err(e) => return string_to_c_char(format!("Error: Failed to create multipart part: {}", e)),
        };
    let form = reqwest::blocking::multipart::Form::new()
        .text("user_secret", user_secret.to_string())
        .text("product_id", package_id.to_string())
        .part("save_file", part);
    
    let client = reqwest::blocking::Client::new();
    let resp = client.post(format!("{}cloud-saves/", URL))
        .multipart(form)
        .send();
    
    match resp {
        Ok(response) => {
            let status = response.status();
            let text = response.text().unwrap_or_else(|_| "No response message".to_string());
            if status.is_success() {
                let parsed: Result<Value, _> = serde_json::from_str(&text);
                if let Ok(json) = parsed {
                    if let Some(msg) = json.get("message") {
                        return string_to_c_char(format!("Upload successful: {}", msg));
                    }
                }
                return string_to_c_char(format!("Upload successful: {}", text));
            } else {
                return string_to_c_char(format!("Upload failed: {}", text));
            }
        }
        Err(e) => {
            return string_to_c_char(format!("Request error: {}", e));
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn download_save_from_server(
    package_id: *const c_char,
    user_secret: *const c_char,
    extract_path: *const c_char
) -> *mut c_char {
    if package_id.is_null() {
        return string_to_c_char("Error: Missing package_id parameter".to_string());
    }
    if user_secret.is_null() {
        return string_to_c_char("Error: Missing user_secret parameter".to_string());
    }
    if extract_path.is_null() {
        return string_to_c_char("Error: Missing extract_path parameter".to_string());
    }
    
    let package_id = unsafe { match CStr::from_ptr(package_id).to_str() {
        Ok(s) if !s.is_empty() => s,
        _ => return string_to_c_char("Error: Invalid package_id parameter".to_string()),
    } };
    let user_secret = unsafe { match CStr::from_ptr(user_secret).to_str() {
        Ok(s) if !s.is_empty() => s,
        _ => return string_to_c_char("Error: Invalid user_secret parameter".to_string()),
    } };
    let extract_path = unsafe { match CStr::from_ptr(extract_path).to_str() {
        Ok(s) if !s.is_empty() => s,
        _ => return string_to_c_char("Error: Invalid extract_path parameter".to_string()),
    } };

    let client = reqwest::blocking::Client::new();
    let resp = client.get(format!("{}cloud-saves/", URL))
        .query(&[ ("user_secret", user_secret), ("product_id", package_id) ])
        .send();
    
    match resp {
        Ok(response) => {
            if response.status().is_success() {
                let bytes = match response.bytes() {
                    Ok(b) => b,
                    Err(e) => return string_to_c_char(format!("Error: Failed to read response bytes: {}", e)),
                };
                let cursor = io::Cursor::new(bytes);
                let mut zip_archive = match zip::ZipArchive::new(cursor) {
                    Ok(z) => z,
                    Err(e) => return string_to_c_char(format!("Error: Failed to open zip archive: {}", e)),
                };
                
                for i in 0..zip_archive.len() {
                    let mut file = match zip_archive.by_index(i) {
                        Ok(f) => f,
                        Err(e) => return string_to_c_char(format!("Error: Failed to access file in zip: {}", e)),
                    };
                    let outpath = Path::new(extract_path).join(file.name());
                    if file.name().ends_with('/') {
                        if let Err(e) = fs::create_dir_all(&outpath) {
                            return string_to_c_char(format!("Error: Failed to create directory: {}", e));
                        }
                    } else {
                        if let Some(p) = outpath.parent() {
                            if !p.exists() {
                                if let Err(e) = fs::create_dir_all(&p) {
                                    return string_to_c_char(format!("Error: Failed to create parent directory: {}", e));
                                }
                            }
                        }
                        let mut outfile = match fs::File::create(&outpath) {
                            Ok(f) => f,
                            Err(e) => return string_to_c_char(format!("Error: Failed to create output file: {}", e)),
                        };
                        if let Err(e) = io::copy(&mut file, &mut outfile) {
                            return string_to_c_char(format!("Error: Failed to copy file contents: {}", e));
                        }
                    }
                }
                return string_to_c_char("Download and extraction successful.".to_string());
            } else {
                let text = response.text().unwrap_or_else(|_| "No response message".to_string());
                return string_to_c_char(format!("Download failed: {}", text));
            }
        }
        Err(e) => {
            return string_to_c_char(format!("Request error: {}", e));
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn get_version_from_id(
    package_id: *const c_char
) -> *mut c_char {
    if package_id.is_null() {
        return string_to_c_char("Error: Missing package_id parameter".to_string());
    }
    
    let package_id = unsafe { match CStr::from_ptr(package_id).to_str() {
        Ok(s) if !s.is_empty() => s,
        _ => return string_to_c_char("Error: Invalid package_id parameter".to_string()),
    } };

    let client = reqwest::blocking::Client::new();
    let resp = client.get(format!("{}version-hex/", URL))
        .query(&[ ("product_id", package_id) ])
        .send();
    
    match resp {
        Ok(response) => {
            if response.status().is_success() {
                let text = response.text().unwrap_or_else(|_| "No response message".to_string());
                let parsed: Result<Value, _> = serde_json::from_str(&text);
                if let Ok(json) = parsed {
                    if let Some(version) = json.get("version") {
                        return string_to_c_char(version.to_string());
                    }
                }
                return string_to_c_char(format!("Response: {}", text));
            } else {
                let text = response.text().unwrap_or_else(|_| "No response message".to_string());
                return string_to_c_char(format!("Request failed: {}", text));
            }
        }
        Err(e) => {
            return string_to_c_char(format!("Request error: {}", e));
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn send_notification(
    title: *const c_char,
    body: *const c_char
) -> *mut c_char {
    if title.is_null() {
        return string_to_c_char("Error: Missing title parameter".to_string());
    }
    if body.is_null() {
        return string_to_c_char("Error: Missing body parameter".to_string());
    }

    let title = unsafe {
        match CStr::from_ptr(title).to_str() {
            Ok(s) if !s.is_empty() => s,
            _ => return string_to_c_char("Error: Invalid title parameter".to_string()),
        }
    };

    let body = unsafe {
        match CStr::from_ptr(body).to_str() {
            Ok(s) if !s.is_empty() => s,
            _ => return string_to_c_char("Error: Invalid body parameter".to_string()),
        }
    };

    if !is_sdl_available() {
        return string_to_c_char("SDL2 is not available on this platform or the SDL2 library not found.".to_string());
    }

    if !is_sdl_initialized() {
        match sdl2::init() {
            Ok(_) => {},
            Err(e) => return string_to_c_char(format!("SDL2 init failed: {}", e)),
        };
    }

    match sdl2::messagebox::show_simple_message_box(
        sdl2::messagebox::MessageBoxFlag::INFORMATION,
        title,
        body,
        None,
    ) {
        Ok(_) => string_to_c_char(format!("Notification sent: {} - {}", title, body)),
        Err(e) => string_to_c_char(format!("SDL2 messagebox failed: {}", e)),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn check_and_show_notification(product_id: *const c_char) -> *mut c_char {
    if product_id.is_null() {
        return string_to_c_char("Error: Missing product_id parameter".to_string());
    }

    let product_id = unsafe {
        match CStr::from_ptr(product_id).to_str() {
            Ok(s) if !s.is_empty() => s,
            _ => return string_to_c_char("Error: Invalid product_id parameter".to_string()),
        }
    };

    let client = reqwest::blocking::Client::new();
    let url = format!("{}get-latest-notification-for-app/?product_id={}", URL, product_id);
 
    let resp = client.get(&url).send();
 
    match resp {
        Ok(resp) => {
            if resp.status().is_success() {
                let text = match resp.text() {
                    Ok(t) => t,
                    Err(e) => return string_to_c_char(format!("Failed to read response text: {}", e)),
                };
                let json: Value = match serde_json::from_str(&text) {
                    Ok(j) => j,
                    Err(e) => return string_to_c_char(format!("Failed to parse JSON: {}", e)),
                };

                let notif_id = json.get("notification_id").and_then(|id| id.as_u64()).unwrap_or(0) as u32;
                let title = json.get("title").and_then(|t| t.as_str()).unwrap_or("Notification");
                let message = json.get("message").and_then(|m| m.as_str()).unwrap_or("");

                if message.is_empty() || notif_id == 0 {
                    return string_to_c_char("No notification to show.".to_string());
                }

                let mut cache = load_notification_cache();
                if cache.contains(&notif_id) {
                    return string_to_c_char("Notification already shown.".to_string());
                }

                let c_title = CString::new(title).unwrap();
                let c_body = CString::new(message).unwrap();

                let _ = send_notification(c_title.as_ptr(), c_body.as_ptr());

                cache.insert(notif_id);
                save_notification_cache(&cache);

                return string_to_c_char("Notification shown.".to_string());
            } else {
                return string_to_c_char("No notification returned from server.".to_string());
            }
        }
        Err(e) => {
            return string_to_c_char(format!("HTTP request failed: {}", e));
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn init_simple_loop(product_id: *const c_char) -> *mut c_char {
    //_local_state_path: *const c_char
    // simple loop, this will be expanded to a more complex loop as the SDK grows.
    if product_id.is_null() {
        return string_to_c_char("Error: Missing product_id parameter".to_string());
    }

    /*if local_state_path.is_null() {
        return string_to_c_char("Error: Missing local_state_path parameter".to_string());
    }

    let local_state_path = unsafe {
        match CStr::from_ptr(local_state_path).to_str() {
            Ok(s) if !s.is_empty() => s,
            _ => return string_to_c_char("Error: Invalid local_state_path parameter".to_string()),
        }
    };

    unsafe { std::env::set_var("LOCAL_STATE_PATH", local_state_path) };*/

    let c_str = unsafe { CStr::from_ptr(product_id) };
    let id = match c_str.to_str() {
        Ok(s) if !s.is_empty() => s.to_owned(),
        _ => return string_to_c_char("Error: Invalid product_id parameter".to_string()),
    };

    std::thread::spawn(move || {
        loop {
            let c_id = CString::new(id.clone()).unwrap();
            let _ = check_and_show_notification(c_id.as_ptr());
            std::thread::sleep(std::time::Duration::from_secs(140));
        }
    });
    
    string_to_c_char("Background notification loop started.".to_string())
}

#[unsafe(no_mangle)]
pub extern "C" fn free_c_string(s: *mut c_char) {
    if s.is_null() { return; }
    unsafe { let _ = CString::from_raw(s); }
}

#[unsafe(no_mangle)]
pub extern "C" fn is_devstore_online() -> *mut c_char {
    let client = reqwest::blocking::Client::new();
    let req = client.get(format!("{}status-check", URL)).send();
    match req {
        Ok(response) => {
            let status = response.status();

            match status.as_u16() {
                200 => string_to_c_char("0".to_string()),
                503 => string_to_c_char("1".to_string()),
                _ => string_to_c_char("2".to_string()),
            }
        }

        Err(_) => {
            string_to_c_char("2".to_string())
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn get_current_username(user_secret: *const c_char) -> *mut c_char {
    if user_secret.is_null() {
        return string_to_c_char("Error: Missing user_secret parameter".to_string());
    }

    let user_secret = unsafe {
        match CStr::from_ptr(user_secret).to_str() {
            Ok(s) if !s.is_empty() => s,
            _ => return string_to_c_char("Error: Invalid user_secret parameter".to_string()),
        }
    };

    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(format!("{}get-username-by-secret/", URL))
        .form(&[("user_secret", user_secret)])
        .send();

    match resp {
        Ok(response) => {
            let status = response.status();
            let text = response
                .text()
                .unwrap_or_else(|_| "No response message".to_string());

            if !status.is_success() {
                return string_to_c_char(format!(
                    "Error: Request failed (status {}): {}",
                    status.as_u16(),
                    text
                ));
            }

            let json: Value = match serde_json::from_str(&text) {
                Ok(j) => j,
                Err(e) => {
                    return string_to_c_char(format!(
                        "Error: Failed to parse response JSON: {}",
                        e
                    ))
                }
            };

            match json.get("status").and_then(Value::as_str) {
                Some("success") => {
                    match json.get("username").and_then(Value::as_str) {
                        Some(username) => string_to_c_char(username.to_string()),
                        None => string_to_c_char("Error: Username missing in response".to_string()),
                    }
                }
                Some("error") => {
                    let msg = json
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Unknown error");
                    string_to_c_char(format!("Error: Server error: {}", msg))
                }
                Some(other) => {
                    string_to_c_char(format!("Error: Unexpected status in response: {}", other))
                }
                None => string_to_c_char("Error: Missing status in response".to_string()),
            }
        }
        Err(e) => {
            string_to_c_char(format!("Error: Network error: {}", e))
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn download_update_for_product(
    package_id: *const c_char
) -> *mut c_char {
    if package_id.is_null() {
        return string_to_c_char("Error: Missing package_id parameter".to_string());
    }
    let package_id = match CStr::from_ptr(package_id).to_str() {
        Ok(s) if !s.is_empty() => s,
        _ => return string_to_c_char("Error: Invalid package_id parameter".to_string()),
    };

    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(format!("{}get-latest-patch/?product_id={}", URL, package_id))
        .send();

    let response = match resp {
        Ok(r) => r,
        Err(e) => {
            return string_to_c_char(format!("Error: Network error: {}", e));
        }
    };

    if !response.status().is_success() {
        let txt = response.text().unwrap_or_else(|_| "No response message".to_string());
        return string_to_c_char(format!("Error: Request failed: {}", txt));
    }

    let bytes = match response.bytes() {
        Ok(b) => b,
        Err(e) => return string_to_c_char(format!("Error: Failed to read response bytes: {}", e)),
    };

    let mut update_path = get_pref_path();    
    update_path.push("update");
    if update_path.exists() {
        if let Err(e) = fs::remove_dir_all(&update_path) {
            return string_to_c_char(format!("Error: Failed to remove old update dir: {}", e));
        }
    }
    if let Err(e) = fs::create_dir_all(&update_path) {
        return string_to_c_char(format!("Error: Failed to create update dir: {}", e));
    }
    let cursor = io::Cursor::new(bytes);
    let mut zip_archive = match zip::ZipArchive::new(cursor) {
        Ok(z) => z,
        Err(e) => return string_to_c_char(format!("Error: Failed to open zip archive: {}", e)),
    };

    for i in 0..zip_archive.len() {
        let mut file = match zip_archive.by_index(i) {
            Ok(f)  => f,
            Err(e) => return string_to_c_char(format!("Error: Failed to access file in zip: {}", e)),
        };
        let outpath = update_path.join(Path::new(file.name()));
        if file.name().ends_with('/') {
            if let Err(e) = fs::create_dir_all(&outpath) {
                return string_to_c_char(format!("Error: Failed to create directory: {}", e));
            }
        } else {
            if let Some(p) = outpath.parent() {
                if !p.exists() && fs::create_dir_all(p).is_err() {
                    return string_to_c_char("Error: Failed to create parent directory".to_string());
                }
            }
            let mut outfile = match fs::File::create(&outpath) {
                Ok(f)  => f,
                Err(e) => return string_to_c_char(format!("Error: Failed to create file: {}", e)),
            };
            if io::copy(&mut file, &mut outfile).is_err() {
                return string_to_c_char("Error: Failed to write file contents".to_string());
            }
        }
    }

    string_to_c_char("Update downloaded and extracted successfully.".to_string())
}

// end of main functions