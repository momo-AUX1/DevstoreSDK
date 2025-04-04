use reqwest;
use serde_json::Value;
use std::ffi::{CStr, CString};
use std::fs::{self, Metadata};
use zip;
use std::io::{self, Write};
use std::os::raw::c_char;
use std::path::Path;
use walkdir::WalkDir;

const URL: &str = "https://xbdev.store/api/cloud-saves/";

fn string_to_c_char(s: String) -> *mut c_char {
    CString::new(s).unwrap().into_raw()
}

#[unsafe(no_mangle)]
pub extern "C" fn free_c_string(s: *mut c_char) {
    if s.is_null() { return; }
    unsafe { let _ = CString::from_raw(s); }
}

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
        .part("save_data", part);
    
    let client = reqwest::blocking::Client::new();
    let resp = client.post(URL)
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
    let resp = client.get(URL)
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