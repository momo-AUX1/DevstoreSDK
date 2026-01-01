use std::env;
use std::fs;
use std::io;
use std::path::Path;

const HEADER_TEMPLATE: &str = r#"#ifndef DEVSTORE_SDK_H
#define DEVSTORE_SDK_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum DevstoreMessageStatus {
    DEVSTORE_MESSAGE_STATUS_INFO = 0,
    DEVSTORE_MESSAGE_STATUS_SUCCESS = 1,
    DEVSTORE_MESSAGE_STATUS_WARNING = 2,
    DEVSTORE_MESSAGE_STATUS_ERROR = 3,
} DevstoreMessageStatus;

typedef struct DevstoreFfiMessage {
    DevstoreMessageStatus status;
    uint32_t code;
    char* message;
} DevstoreFfiMessage;

DevstoreFfiMessage* get_sdk_version(void);
DevstoreFfiMessage* set_custom_url(const char* custom_url);
DevstoreFfiMessage* upload_save_to_server(const char* package_id, const char* user_secret, const char* file_or_folder_path);
DevstoreFfiMessage* download_save_from_server(const char* package_id, const char* user_secret, const char* extract_path);
DevstoreFfiMessage* get_version_from_id(const char* package_id);
DevstoreFfiMessage* send_notification(const char* title, const char* body);
DevstoreFfiMessage* check_and_show_notification(const char* product_id);
DevstoreFfiMessage* init_simple_loop(const char* product_id);
DevstoreFfiMessage* is_devstore_online(void);
DevstoreFfiMessage* get_current_username(const char* user_secret);
DevstoreFfiMessage* download_update_for_product(const char* package_id);
DevstoreFfiMessage* verify_download_v2(const char* package_id);
void devstore_free_message(DevstoreFfiMessage* message);

#ifdef __cplusplus
}
#endif

#endif // DEVSTORE_SDK_H
"#;

fn main() -> io::Result<()> {
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".to_string());
    let include_dir = Path::new("include");
    fs::create_dir_all(include_dir)?;
    let header_path = include_dir.join("devstore_sdk.h");
    let contents = format!(
        "/* Auto-generated devstoreSDK header v{} */\n{}\n",
        version, HEADER_TEMPLATE
    );
    fs::write(&header_path, contents)?;
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    Ok(())
}
