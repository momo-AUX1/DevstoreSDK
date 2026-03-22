/* Auto-generated devstoreSDK header v0.4.9 */
#ifndef DEVSTORE_SDK_H
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
DevstoreFfiMessage* verify_download_code(const char* product_id, const char* code);
DevstoreFfiMessage* verify_resigned_install_token(const char* product_id, const char* install_token);
DevstoreFfiMessage* verify_resigned_package_path(const char* product_id, const char* package_or_root_path);
DevstoreFfiMessage* init_sdk_for_user(const char* product_id, const char* secret_code);
DevstoreFfiMessage* set_presence_for_user(const char* details);
DevstoreFfiMessage* discord_heartbeat(void);
DevstoreFfiMessage* discord_quit(void);
void devstore_free_message(DevstoreFfiMessage* message);

#ifdef __cplusplus
}
#endif

#endif // DEVSTORE_SDK_H

