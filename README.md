# Dev Store SDK

A Rust-based SDK for interacting with the Dev Store API. This SDK simplifies access to dev store features, allowing apps, emulators, and games to easily integrate with Dev Store services.

## Version 1.0 - Cloud Saves Update

Version 1.0 introduces cloud save functionality! Now you can securely store and retrieve game save data in the cloud, giving users peace of mind knowing their progress is safely backed up with their Dev Store account.

## Features

- 🎮 Download games from the Dev Store
- ☁️ Cloud save synchronization
- 🔄 Upload local save files or entire directories
- 📥 Download and restore save data
- 🔐 Secure API authentication

## Requirements

- Rust 1.56.0 or higher (for building)
- Dev Store account
- Valid package ID and user secret

## Installation

The SDK is distributed as a DLL (Dynamic Link Library) that can be integrated with various programming languages.

## Using the SDK

### In Rust

```rust
use libloading::{Library, Symbol};
use std::ffi::CString;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load the DLL
    let lib = Library::new("devstoreSDK.dll")?;
    
    // Get function pointers
    unsafe {
        let upload_save_to_server: Symbol<unsafe extern "C" fn(
            *const std::os::raw::c_char,
            *const std::os::raw::c_char,
            *const std::os::raw::c_char
        ) -> *mut std::os::raw::c_char> = lib.get(b"upload_save_to_server")?;
        
        let free_c_string: Symbol<unsafe extern "C" fn(*mut std::os::raw::c_char)> = 
            lib.get(b"free_c_string")?;
        
        // Use the functions
        let package_id = CString::new("your-package-id")?;
        let user_secret = CString::new("your-user-secret")?;
        let save_path = CString::new("/path/to/your/save/file/or/folder")?;
        
        let ptr = upload_save_to_server(
            package_id.as_ptr(),
            user_secret.as_ptr(),
            save_path.as_ptr()
        );
        
        let response = CString::from_raw(ptr).into_string()?;
        println!("{}", response);
        
        free_c_string(ptr);
    }
    
    Ok(())
}
```

### In C++

```cpp
#include <iostream>
#include <windows.h>
#include <string>

typedef char* (*UploadSaveFunc)(const char*, const char*, const char*);
typedef void (*FreeStringFunc)(char*);

int main() {
    // Load the DLL
    HINSTANCE hDLL = LoadLibrary("devstoreSDK.dll");
    if (!hDLL) {
        std::cout << "Failed to load DLL" << std::endl;
        return 1;
    }
    
    // Get function pointers
    UploadSaveFunc uploadSave = (UploadSaveFunc)GetProcAddress(hDLL, "upload_save_to_server");
    FreeStringFunc freeString = (FreeStringFunc)GetProcAddress(hDLL, "free_c_string");
    
    if (!uploadSave || !freeString) {
        std::cout << "Failed to get function addresses" << std::endl;
        FreeLibrary(hDLL);
        return 1;
    }
    
    // Use the functions
    const char* packageId = "your-package-id";
    const char* userSecret = "your-user-secret";
    const char* savePath = "C:\\path\\to\\save\\file";
    
    char* result = uploadSave(packageId, userSecret, savePath);
    std::cout << "Result: " << result << std::endl;
    freeString(result);
    
    FreeLibrary(hDLL);
    return 0;
}
```

### In C#

```csharp
using System;
using System.Runtime.InteropServices;

class DevStoreSDK
{
    [DllImport("devstoreSDK.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr upload_save_to_server(
        [MarshalAs(UnmanagedType.LPStr)] string packageId,
        [MarshalAs(UnmanagedType.LPStr)] string userSecret,
        [MarshalAs(UnmanagedType.LPStr)] string fileOrFolderPath);
        
    [DllImport("devstoreSDK.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern void free_c_string(IntPtr str);
    
    [DllImport("devstoreSDK.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr download_save_from_server(
        [MarshalAs(UnmanagedType.LPStr)] string packageId,
        [MarshalAs(UnmanagedType.LPStr)] string userSecret,
        [MarshalAs(UnmanagedType.LPStr)] string extractPath);
}

class Program
{
    static void Main()
    {
        // Upload example
        string packageId = "your-package-id";
        string userSecret = "your-user-secret";
        string savePath = @"C:\path\to\save\file";
        
        IntPtr resultPtr = DevStoreSDK.upload_save_to_server(packageId, userSecret, savePath);
        string result = Marshal.PtrToStringAnsi(resultPtr);
        Console.WriteLine(result);
        DevStoreSDK.free_c_string(resultPtr);
        
        // Download example
        string extractPath = @"C:\path\to\extract";
        resultPtr = DevStoreSDK.download_save_from_server(packageId, userSecret, extractPath);
        result = Marshal.PtrToStringAnsi(resultPtr);
        Console.WriteLine(result);
        DevStoreSDK.free_c_string(resultPtr);
    }
}
```

## API Reference

### upload_save_to_server

Uploads save data (file or folder) to the Dev Store cloud.

Parameters:
- `package_id`: Your application's package ID
- `user_secret`: User's authentication token
- `file_or_folder_path`: Path to the save file or directory to upload

Returns:
- Success/failure message as a C string (must be freed with `free_c_string`)

### download_save_from_server

Downloads save data from the Dev Store cloud.

Parameters:
- `package_id`: Your application's package ID
- `user_secret`: User's authentication token
- `extract_path`: Directory path where to extract the downloaded save data

Returns:
- Success/failure message as a C string (must be freed with `free_c_string`)

### free_c_string

Frees memory allocated for strings returned by other functions.

Parameters:
- `s`: Pointer to the string to free

## Building from Source

```bash
cargo build --release
```

The compiled DLL will be in the `target/release` directory.

## License

The SDK is licensed under the MIT license.

---

For more information and support, visit the [Dev Store](https://xbdev.store).