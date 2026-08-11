use crate::log::{LogLine, push_log};
use eframe::egui;
use self_update::update::ReleaseUpdate;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

// launcher is only built for arm mac, x86 linux, and x86 windows
// name of the release asset (a compressed archive) on GitHub
#[cfg(target_os = "macos")]
const PROXHY_ASSET_NAME: &str = "proxhy-aarch64-apple-darwin.tar.gz";
#[cfg(target_os = "linux")]
const PROXHY_ASSET_NAME: &str = "proxhy-x86_64-unknown-linux-gnu.tar.gz";
#[cfg(target_os = "windows")]
const PROXHY_ASSET_NAME: &str = "proxhy-x86_64-pc-windows-msvc.zip";

// name of the binary inside the archive, and once installed locally
#[cfg(target_os = "windows")]
const PROXHY_BIN_NAME: &str = "proxhy.exe";
#[cfg(not(target_os = "windows"))]
const PROXHY_BIN_NAME: &str = "proxhy";

fn proxhy_data_dir() -> PathBuf {
    let dir = dirs::data_dir()
        .expect("no platform data dir")
        .join("proxhy");
    std::fs::create_dir_all(&dir).ok();
    dir
}

pub fn proxhy_binary_path() -> PathBuf {
    proxhy_data_dir().join(PROXHY_BIN_NAME)
}

fn proxhy_archive_path() -> PathBuf {
    proxhy_data_dir().join(PROXHY_ASSET_NAME)
}

fn proxhy_version_path() -> PathBuf {
    proxhy_data_dir().join("version.txt")
}

fn read_installed_proxhy_version() -> Option<String> {
    std::fs::read_to_string(proxhy_version_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn write_installed_proxhy_version(version: &str) {
    let _ = std::fs::write(proxhy_version_path(), version);
}

// --- HTTP / download ---

fn fetch_latest_proxhy_version() -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("proxhy-launcher")
        .build()
        .map_err(|e| e.to_string())?;
    let resp: serde_json::Value = client
        .get("https://api.github.com/repos/proxhyhq/proxhy/releases/latest")
        .send()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    resp["tag_name"]
        .as_str()
        .ok_or_else(|| "missing tag_name in API response".to_string())
        .map(|s| s.trim_start_matches('v').to_string())
}

#[cfg(target_os = "windows")]
fn extract_proxhy_binary(
    archive_path: &std::path::Path,
    dest: &std::path::Path,
) -> Result<(), String> {
    let file = std::fs::File::open(archive_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;

    let mut found = false;
    for i in 0..archive.len() {
        let mut file_in_zip = archive.by_index(i).map_err(|e| e.to_string())?;
        if file_in_zip.name().ends_with(PROXHY_BIN_NAME) {
            let mut output = std::fs::File::create(dest).map_err(|e| e.to_string())?;
            std::io::copy(&mut file_in_zip, &mut output).map_err(|e| e.to_string())?;
            found = true;
            break;
        }
    }

    if !found {
        return Err(format!("Binary {PROXHY_BIN_NAME} not found in archive"));
    }

    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn extract_proxhy_binary(
    archive_path: &std::path::Path,
    dest: &std::path::Path,
) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let file = std::fs::File::open(archive_path).map_err(|e| e.to_string())?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    let mut found = false;
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let matches = {
            let path = entry.path().map_err(|e| e.to_string())?;
            path.file_name() == Some(std::ffi::OsStr::new(PROXHY_BIN_NAME))
        };
        if matches {
            let mut output = std::fs::File::create(dest).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut output).map_err(|e| e.to_string())?;
            found = true;
            break;
        }
    }

    if !found {
        return Err(format!("Binary {PROXHY_BIN_NAME} not found in archive"));
    }

    std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| e.to_string())?;

    Ok(())
}

fn download_proxhy_binary(
    version: &str,
    log: &Arc<Mutex<VecDeque<LogLine>>>,
    ctx: &egui::Context,
) -> Result<(), String> {
    let url = format!(
        "https://github.com/proxhyhq/proxhy/releases/download/v{version}/{PROXHY_ASSET_NAME}"
    );
    let archive_dest = proxhy_archive_path();
    let bin_dest = proxhy_binary_path();
    push_log(log, &format!("[gui] Downloading {url}..."), ctx);

    let client = reqwest::blocking::Client::builder()
        .user_agent("proxhy-launcher")
        .build()
        .map_err(|e| e.to_string())?;
    let mut resp = client.get(&url).send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {} for {url}", resp.status()));
    }

    let total = resp.content_length();
    let tmp = archive_dest.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 65536];
    let mut downloaded: u64 = 0;
    let mut last_bucket: Option<u64> = None;

    loop {
        let n = resp.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        downloaded += n as u64;
        if let Some(t) = total {
            let pct = downloaded * 100 / t;
            let bucket = pct / 5;
            if last_bucket != Some(bucket) {
                push_log(log, &format!("[gui] {pct}% ({downloaded}/{t} bytes)"), ctx);
                last_bucket = Some(bucket);
            }
        }
    }
    drop(file);

    std::fs::rename(&tmp, &archive_dest).map_err(|e| e.to_string())?;
    push_log(
        log,
        &format!("[gui] Download complete ({downloaded} bytes). Extracting..."),
        ctx,
    );

    extract_proxhy_binary(&archive_dest, &bin_dest)?;

    std::fs::remove_file(&archive_dest).ok();
    push_log(log, "[gui] Extraction complete.", ctx);
    Ok(())
}

// --- update state ---

#[derive(Default, Clone)]
pub struct UpdateState {
    pub gui_available: Option<String>,
    pub installing: bool,
    pub error: Option<String>,
}

#[derive(Default, Clone)]
pub struct ProxhyState {
    pub installed_version: Option<String>,
    pub latest_version: Option<String>,
    pub updating: bool,
    pub error: Option<String>,
}

impl ProxhyState {
    pub fn update_available(&self) -> bool {
        match (&self.installed_version, &self.latest_version) {
            (Some(installed), Some(latest)) => installed != latest,
            (None, Some(_)) => true,
            _ => false,
        }
    }
}

fn gui_updater() -> Result<Box<dyn ReleaseUpdate>, self_update::errors::Error> {
    self_update::backends::github::Update::configure()
        .repo_owner("proxhyhq")
        .repo_name("launcher")
        .bin_name("proxhy-launcher")
        .current_version(env!("CARGO_PKG_VERSION"))
        .no_confirm(true)
        .build()
}

fn appimage_path() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE").map(PathBuf::from)
}

fn fetch_latest_launcher_version() -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("proxhy-launcher")
        .build()
        .map_err(|e| e.to_string())?;
    let resp: serde_json::Value = client
        .get("https://api.github.com/repos/proxhyhq/launcher/releases/latest")
        .send()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    resp["tag_name"]
        .as_str()
        .ok_or_else(|| "missing tag_name in API response".to_string())
        .map(|s| s.trim_start_matches('v').to_string())
}

fn update_appimage(appimage_path: &std::path::Path) -> Result<(), String> {
    let version = fetch_latest_launcher_version()?;
    let url = format!(
        "https://github.com/proxhyhq/launcher/releases/download/v{version}/Proxhy.AppImage"
    );

    let client = reqwest::blocking::Client::builder()
        .user_agent("proxhy-launcher")
        .build()
        .map_err(|e| e.to_string())?;
    let mut resp = client.get(&url).send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {} for {url}", resp.status()));
    }

    // download next to the target so the final rename is atomic (same filesystem)
    let tmp = appimage_path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    std::io::copy(&mut resp, &mut file).map_err(|e| e.to_string())?;
    drop(file);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }

    std::fs::rename(&tmp, appimage_path).map_err(|e| e.to_string())?;
    Ok(())
}

// --- background startup tasks ---

pub fn spawn_ensure_binary(
    log: Arc<Mutex<VecDeque<LogLine>>>,
    proxhy_state: Arc<Mutex<ProxhyState>>,
    ctx: egui::Context,
) {
    proxhy_state.lock().unwrap().installed_version = read_installed_proxhy_version();
    if proxhy_binary_path().exists() {
        return;
    }
    thread::spawn(move || {
        push_log(&log, "[gui] Proxhy not found. Downloading...", &ctx);
        proxhy_state.lock().unwrap().updating = true;
        let result = fetch_latest_proxhy_version()
            .and_then(|version| download_proxhy_binary(&version, &log, &ctx).map(|()| version));
        let mut s = proxhy_state.lock().unwrap();
        s.updating = false;
        match result {
            Ok(version) => {
                write_installed_proxhy_version(&version);
                s.installed_version = Some(version.clone());
                s.latest_version = Some(version);
                drop(s);
                push_log(&log, "[gui] Proxhy ready.", &ctx);
            }
            Err(e) => {
                s.error = Some(e.clone());
                drop(s);
                push_log(&log, &format!("[gui] Download failed: {e}"), &ctx);
            }
        }
    });
}

pub fn spawn_gui_update_check(state: Arc<Mutex<UpdateState>>) {
    thread::spawn(
        move || match gui_updater().and_then(|u| u.get_latest_release()) {
            Ok(release) if release.version != env!("CARGO_PKG_VERSION") => {
                state.lock().unwrap().gui_available = Some(release.version);
            }
            Ok(_) => {}
            Err(e) => {
                state.lock().unwrap().error = Some(format!("GUI update check: {e}"));
            }
        },
    );
}

pub fn spawn_proxhy_update_check(proxhy_state: Arc<Mutex<ProxhyState>>) {
    thread::spawn(move || match fetch_latest_proxhy_version() {
        Ok(version) => proxhy_state.lock().unwrap().latest_version = Some(version),
        Err(e) => {
            proxhy_state.lock().unwrap().error = Some(format!("proxhy update check: {e}"));
        }
    });
}

// --- proxhy self update ---

pub fn run_proxhy_update(
    proxhy_state: Arc<Mutex<ProxhyState>>,
    log: Arc<Mutex<VecDeque<LogLine>>>,
    ctx: egui::Context,
) {
    thread::spawn(move || {
        proxhy_state.lock().unwrap().updating = true;
        push_log(&log, "[gui] Checking for updates...", &ctx);

        let result = fetch_latest_proxhy_version()
            .and_then(|version| download_proxhy_binary(&version, &log, &ctx).map(|()| version));

        let mut s = proxhy_state.lock().unwrap();
        s.updating = false;
        match result {
            Ok(version) => {
                write_installed_proxhy_version(&version);
                s.installed_version = Some(version.clone());
                s.latest_version = Some(version);
                drop(s);
                push_log(&log, "[gui] Update complete. Please restart proxhy.", &ctx);
            }
            Err(e) => {
                s.error = Some(e.clone());
                drop(s);
                push_log(&log, &format!("[gui] Update failed: {e}"), &ctx);
            }
        }
    });
}

// --- GUI update ---

pub fn apply_gui_update(
    state: Arc<Mutex<UpdateState>>,
    log: Arc<Mutex<VecDeque<LogLine>>>,
    ctx: egui::Context,
) {
    thread::spawn(move || {
        state.lock().unwrap().installing = true;
        push_log(&log, "[gui] Updating proxhy-launcher...", &ctx);

        let result = appimage_path().map_or_else(
            || {
                gui_updater()
                    .and_then(|u| u.update())
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            },
            |path| update_appimage(&path),
        );

        let mut s = state.lock().unwrap();
        s.installing = false;
        match result {
            Ok(()) => {
                s.gui_available = None;
                drop(s);
                push_log(&log, "[gui] GUI updated — please restart.", &ctx);
            }
            Err(e) => {
                s.error = Some(e.clone());
                drop(s);
                push_log(&log, &format!("[gui] GUI update failed: {e}"), &ctx);
            }
        }
    });
}
