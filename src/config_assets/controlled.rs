//! Refuse asset projection when unsupported controlled config could change it.
use super::{FILE_SETTINGS, read_config};
#[cfg(target_os = "macos")]
use anyhow::Context;
use anyhow::{Result, bail};
use std::path::PathBuf;

fn guard_config(config: &toml::Value, source: &str) -> Result<()> {
    let relevant = FILE_SETTINGS.iter().any(|key| config.get(key).is_some())
        || ["project_root_markers", "projects", "log_dir", "sqlite_home"]
            .iter()
            .any(|key| config.get(key).is_some())
        || config
            .get("agents")
            .and_then(toml::Value::as_table)
            .is_some_and(|roles| roles.values().any(|role| role.get("config_file").is_some()));
    if relevant {
        bail!(
            "cannot safely preserve relative shared config assets with path, runtime-location or project-trust settings in {source}; use absolute file references in the shared user config, or launch Codex directly"
        );
    }
    Ok(())
}

pub(super) fn guard_relocation() -> Result<()> {
    #[cfg(unix)]
    let files = [
        PathBuf::from("/etc/codex/config.toml"),
        PathBuf::from("/etc/codex/managed_config.toml"),
    ];
    #[cfg(windows)]
    let files = [windows_system_dir().join("config.toml")];
    for file in files {
        if file.exists() {
            // Never open a special file while inspecting a controlled layer.
            if !std::fs::metadata(&file)?.is_file() {
                bail!(
                    "cannot inspect controlled Codex config {}; use absolute shared file references",
                    file.display()
                );
            }
            guard_config(&read_config(&file)?, &file.display().to_string())?;
        }
    }
    #[cfg(target_os = "macos")]
    if let Some(encoded) = macos_preference()? {
        use base64::Engine;
        let contents = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .context("decode managed Codex config (contents omitted)")?;
        let config = toml::from_str(std::str::from_utf8(&contents)?)
            .map_err(|_| anyhow::anyhow!("invalid managed Codex config (contents omitted)"))?;
        guard_config(&config, "com.openai.codex:config_toml_base64")?;
    }
    Ok(())
}

#[cfg(windows)]
fn windows_system_dir() -> PathBuf {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt, ptr};
    use windows_sys::Win32::{
        System::Com::CoTaskMemFree,
        UI::Shell::{FOLDERID_ProgramData, SHGetKnownFolderPath},
    };
    let mut wide = ptr::null_mut();
    // SAFETY: the OS initializes a null-terminated CoTaskMem string on success.
    let status =
        unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, 0, ptr::null_mut(), &mut wide) };
    if status != 0 || wide.is_null() {
        if !wide.is_null() {
            unsafe { CoTaskMemFree(wide.cast()) };
        }
        return PathBuf::from(r"C:\ProgramData\OpenAI\Codex");
    }
    let path = unsafe {
        let mut length = 0;
        while *wide.add(length) != 0 {
            length += 1;
        }
        let path = PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(
            wide, length,
        )));
        CoTaskMemFree(wide.cast());
        path
    };
    path.join("OpenAI").join("Codex")
}

#[cfg(target_os = "macos")]
fn macos_preference() -> Result<Option<String>> {
    use std::{
        ffi::{CStr, c_char, c_void},
        ptr,
    };
    const UTF8: u32 = 0x0800_0100;
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            text: *const c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFPreferencesCopyAppValue(key: *const c_void, app: *const c_void) -> *const c_void;
        fn CFGetTypeID(value: *const c_void) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFStringGetLength(value: *const c_void) -> isize;
        fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
        fn CFStringGetCString(
            value: *const c_void,
            buffer: *mut c_char,
            size: isize,
            encoding: u32,
        ) -> u8;
        fn CFRelease(value: *const c_void);
    }
    struct Owned(*const c_void);
    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: Owned only holds non-null references returned by Create/Copy.
            unsafe { CFRelease(self.0) };
        }
    }
    fn string(text: &CStr) -> Result<Owned> {
        // SAFETY: the input is terminated and CoreFoundation copies its contents.
        let value = unsafe { CFStringCreateWithCString(ptr::null(), text.as_ptr(), UTF8) };
        if value.is_null() {
            bail!("create managed preference key");
        }
        Ok(Owned(value))
    }
    let key = string(c"config_toml_base64")?;
    let app = string(c"com.openai.codex")?;
    // Use the same preference API/domain/key as Codex, including managed profiles.
    let value = unsafe { CFPreferencesCopyAppValue(key.0, app.0) };
    if value.is_null() {
        return Ok(None);
    }
    let value = Owned(value);
    // SAFETY: check the CF object's type before calling string-only functions.
    unsafe {
        if CFGetTypeID(value.0) != CFStringGetTypeID() {
            bail!("managed Codex preference is not a string");
        }
        let length = CFStringGetMaximumSizeForEncoding(CFStringGetLength(value.0), UTF8);
        if !(0..=1024 * 1024).contains(&length) {
            bail!(
                "managed Codex preference is too large to inspect; use absolute shared file references"
            );
        }
        let mut bytes = vec![0u8; length as usize + 1];
        if CFStringGetCString(
            value.0,
            bytes.as_mut_ptr().cast(),
            bytes.len() as isize,
            UTF8,
        ) == 0
        {
            bail!("read managed Codex preference string");
        }
        Ok(Some(
            CStr::from_ptr(bytes.as_ptr().cast()).to_str()?.to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_only_refuses_settings_that_can_affect_asset_projection() {
        for text in [
            "model = 'fixture'\n",
            "sandbox_mode = 'read-only'\n",
            "[agents]\nmax_threads = 2\n",
            "[features]\nshell_tool = false\n",
        ] {
            guard_config(&toml::from_str(text).unwrap(), "synthetic control").unwrap();
        }
        for text in [
            "model_instructions_file = '/synthetic/instructions.md'\n",
            "model_catalog_json = '/synthetic/models.json'\n",
            "experimental_compact_prompt_file = '/synthetic/compact.md'\n",
            "project_root_markers = ['.git']\n",
            "log_dir = '/synthetic/log'\n",
            "sqlite_home = '/synthetic/sqlite'\n",
            "[projects.'/synthetic/project']\ntrust_level = 'untrusted'\n",
            "[agents.reviewer]\nconfig_file = '/synthetic/role.toml'\n",
        ] {
            let error = guard_config(&toml::from_str(text).unwrap(), "synthetic control")
                .unwrap_err()
                .to_string();
            assert!(error.contains("synthetic control"));
            assert!(error.contains("absolute file references"));
        }
    }
}
