use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;

use crate::db::SqliteDbState;
use crate::http_client;

/// Response from GitHub latest.json
#[derive(Debug, Serialize, Deserialize)]
struct LatestRelease {
    version: String,
    notes: Option<String>,
    pub_date: Option<String>,
    platforms: HashMap<String, PlatformInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PlatformInfo {
    signature: Option<String>,
    url: Option<String>,
}

/// Update check result
#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateCheckResult {
    pub has_update: bool,
    pub current_version: String,
    pub latest_version: String,
    pub release_url: String,
    pub release_notes: String,
    pub signature: Option<String>,
    pub url: Option<String>,
    /// Whether the running app is managed by Scoop. When true the in-app
    /// installer payload is dropped (see `check_for_updates`) and the user
    /// must upgrade via `scoop update ai-toolbox`.
    pub scoop_install: bool,
}

/// Check for updates from GitHub releases.
///
/// Uses the DB-backed proxy client when the database is available (normal
/// startup). When the database is not managed yet (recovery / "schema too
/// new" startup path), falls back to `create_client_with_env_proxy` which
/// honors `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` environment variables.
#[tauri::command]
pub async fn check_for_updates(app: tauri::AppHandle) -> Result<UpdateCheckResult, String> {
    const GITHUB_REPO: &str = "reisen-ww/codex-gateway-router";
    let latest_json_url = format!(
        "https://github.com/{}/releases/latest/download/latest.json",
        GITHUB_REPO
    );

    // Get current version from package info
    let current_version = app.package_info().version.to_string();

    // Detect current platform
    let current_platform = detect_current_platform();

    // Fetch latest.json. Prefer DB-backed proxy client; in recovery mode
    // (DB not managed) use an env-proxy client.
    let response = match app.try_state::<SqliteDbState>() {
        Some(state) => {
            let client = http_client::client(&state).await?;
            client.get(&latest_json_url).send().await
        }
        None => {
            let client = http_client::create_client_with_env_proxy(30)?;
            client.get(&latest_json_url).send().await
        }
    }
    .map_err(|e| format!("Failed to fetch latest.json: {}", e))?;

    if !response.status().is_success() {
        return Err(format!(
            "Failed to fetch latest.json: HTTP {}",
            response.status()
        ));
    }

    let release: LatestRelease = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse latest.json: {}", e))?;

    let latest_version = release.version.trim_start_matches('v').to_string();

    let has_update = compare_versions(&latest_version, &current_version) > 0;

    // Get signature and url for current platform
    let platform_info = release.platforms.get(&current_platform);
    let mut signature = platform_info
        .and_then(|p| p.signature.clone())
        .filter(|s| !s.is_empty());
    let mut url = platform_info
        .and_then(|p| p.url.clone())
        .filter(|s| !s.is_empty());

    let scoop_install = is_scoop_install();

    // A Scoop-managed install lives under `<scoop>\apps\...`; running the
    // in-app NSIS updater would create a second copy outside Scoop's control
    // while the Scoop-managed copy stays stale. Drop the installer payload so
    // every frontend path degrades to opening the release page instead.
    if scoop_install {
        signature = None;
        url = None;
    }

    Ok(UpdateCheckResult {
        has_update,
        current_version,
        latest_version: latest_version.clone(),
        release_url: format!(
            "https://github.com/{}/releases/tag/v{}",
            GITHUB_REPO, latest_version
        ),
        release_notes: release.notes.unwrap_or_default(),
        signature,
        url,
        scoop_install,
    })
}

/// Whether the running executable is managed by Scoop (Windows package manager).
#[allow(unreachable_code)]
pub fn is_scoop_install() -> bool {
    #[cfg(target_os = "windows")]
    {
        if let Ok(exe_path) = std::env::current_exe() {
            let roots: Vec<String> = ["SCOOP", "SCOOP_GLOBAL"]
                .into_iter()
                .filter_map(|name| std::env::var_os(name))
                .map(|path| path.to_string_lossy().into_owned())
                .collect();
            return is_scoop_install_path(&exe_path.to_string_lossy(), &roots);
        }
        return false;
    }

    false
}

/// Pure path check for Scoop-managed executables (case-insensitive).
#[cfg(any(target_os = "windows", test))]
fn is_scoop_install_path(exe_path: &str, roots: &[String]) -> bool {
    let normalize = |path: &str| {
        let normalized = path.replace('/', "\\").to_lowercase();
        let normalized = if let Some(suffix) = normalized.strip_prefix(r"\\?\unc\") {
            format!(r"\\{suffix}")
        } else {
            normalized
                .strip_prefix(r"\\?\")
                .unwrap_or(&normalized)
                .to_string()
        };
        normalized.trim_end_matches('\\').to_string()
    };
    let normalized_exe = normalize(exe_path);
    normalized_exe.contains("\\scoop\\apps\\")
        || roots
            .iter()
            .filter(|root| !root.trim().is_empty())
            .any(|root| normalized_exe.starts_with(&format!("{}\\apps\\", normalize(root))))
}

/// Detect current platform string for matching latest.json
#[allow(unreachable_code)]
fn detect_current_platform() -> String {
    #[cfg(target_os = "windows")]
    {
        return "windows-x86_64".to_string();
    }

    #[cfg(target_os = "linux")]
    {
        return "linux-x86_64".to_string();
    }

    #[cfg(target_os = "macos")]
    {
        #[cfg(target_arch = "aarch64")]
        {
            return "darwin-aarch64".to_string();
        }
        #[cfg(target_arch = "x86_64")]
        {
            return "darwin-x86_64".to_string();
        }
    }

    "unknown".to_string()
}

/// Run the updater plugin: check for an available update and, if present,
/// download + install it while emitting `update-download-progress` events.
///
/// This is DB-free; proxy handling is the caller's responsibility (the
/// `tauri-plugin-updater` reads `HTTP_PROXY` / `HTTPS_PROXY` env vars).
async fn run_updater_download(app: &tauri::AppHandle) -> Result<bool, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| format!("Failed to check for updates: {}", e))?;

    match update {
        Some(update) => {
            // Emit download started event
            let _ = app.emit(
                "update-download-progress",
                serde_json::json!({
                    "status": "started",
                    "progress": 0,
                    "downloaded": 0,
                    "total": 0,
                    "speed": 0
                }),
            );

            // Download and install with speed calculation
            let downloaded = AtomicU64::new(0);
            let mut last_downloaded = 0u64;
            let mut last_time = Instant::now();
            let mut speed: f64 = 0.0;

            let install_result = update
                .download_and_install(
                    |chunk_length, content_length| {
                        downloaded.fetch_add(chunk_length as u64, Ordering::SeqCst);
                        let current_downloaded = downloaded.load(Ordering::SeqCst);

                        // Calculate download speed
                        let now = Instant::now();
                        let elapsed = now.duration_since(last_time);

                        if elapsed >= Duration::from_millis(200) {
                            let bytes_since_last =
                                current_downloaded.saturating_sub(last_downloaded);
                            if bytes_since_last > 0 {
                                // Speed in bytes per second
                                let speed_calc = bytes_since_last as f64 / elapsed.as_secs_f64();
                                // Use exponential moving average for smoother display
                                if speed == 0.0 {
                                    speed = speed_calc;
                                } else {
                                    speed = speed * 0.7 + speed_calc * 0.3;
                                }
                            }
                            last_downloaded = current_downloaded;
                            last_time = now;
                        }

                        if let Some(total) = content_length {
                            let percentage =
                                (current_downloaded as f64 / total as f64 * 100.0) as u32;
                            // Emit progress event with speed
                            let _ = app.emit(
                                "update-download-progress",
                                serde_json::json!({
                                    "status": "downloading",
                                    "progress": percentage,
                                    "downloaded": current_downloaded,
                                    "total": total,
                                    "speed": speed as u64
                                }),
                            );
                        }
                    },
                    || {
                        let current_downloaded = downloaded.load(Ordering::SeqCst);
                        // Emit installing event
                        let _ = app.emit(
                            "update-download-progress",
                            serde_json::json!({
                                "status": "installing",
                                "progress": 100,
                                "downloaded": current_downloaded,
                                "total": current_downloaded,
                                "speed": 0
                            }),
                        );
                    },
                )
                .await;

            match install_result {
                Ok(_) => {
                    println!("Update installed successfully");
                    Ok(true)
                }
                Err(e) => {
                    let error_msg = format!("Failed to install update: {}", e);
                    eprintln!("{}", error_msg);
                    Err(error_msg)
                }
            }
        }
        None => Err("No update available".to_string()),
    }
}

/// Install the update.
///
/// When the database is available (normal startup), the in-app proxy config is
/// read from the DB and applied to `HTTP_PROXY` / `HTTPS_PROXY` env vars so
/// the updater plugin picks it up. In recovery mode (DB not managed), env
/// vars are left untouched and the updater inherits system / environment
/// proxy settings.
#[tauri::command]
pub async fn install_update(app: tauri::AppHandle) -> Result<bool, String> {
    if is_scoop_install() {
        return Err(
            "Scoop-managed installations must be updated with scoop update ai-toolbox".to_string(),
        );
    }
    // Snapshot proxy-related env vars so we can always restore them, regardless
    // of whether we touched them (no-op restore in recovery mode).
    let old_http_proxy = std::env::var("HTTP_PROXY").ok();
    let old_https_proxy = std::env::var("HTTPS_PROXY").ok();
    let old_http_proxy_lower = std::env::var("http_proxy").ok();
    let old_https_proxy_lower = std::env::var("https_proxy").ok();

    // Only apply DB-backed proxy when the database is actually available.
    if let Some(state) = app.try_state::<SqliteDbState>() {
        let (proxy_mode, proxy_url) = http_client::get_proxy_from_settings(&state).await?;

        // Set proxy environment variables for the updater plugin
        // (tauri-plugin-updater reads these env vars for proxy configuration)
        match proxy_mode {
            http_client::ProxyMode::Direct => {
                std::env::remove_var("HTTP_PROXY");
                std::env::remove_var("HTTPS_PROXY");
                std::env::remove_var("http_proxy");
                std::env::remove_var("https_proxy");
            }
            http_client::ProxyMode::Custom => {
                if !proxy_url.is_empty() {
                    std::env::set_var("HTTP_PROXY", &proxy_url);
                    std::env::set_var("HTTPS_PROXY", &proxy_url);
                    std::env::set_var("http_proxy", &proxy_url);
                    std::env::set_var("https_proxy", &proxy_url);
                }
            }
            http_client::ProxyMode::System => {}
        }
    }
    // else: recovery mode — leave env vars as-is (system / env proxy).

    let result = run_updater_download(&app).await;

    // Restore original environment variables
    if let old @ Some(_) = old_http_proxy {
        std::env::set_var("HTTP_PROXY", old.unwrap());
    } else {
        std::env::remove_var("HTTP_PROXY");
    }
    if let old @ Some(_) = old_https_proxy {
        std::env::set_var("HTTPS_PROXY", old.unwrap());
    } else {
        std::env::remove_var("HTTPS_PROXY");
    }
    if let old @ Some(_) = old_http_proxy_lower {
        std::env::set_var("http_proxy", old.unwrap());
    } else {
        std::env::remove_var("http_proxy");
    }
    if let old @ Some(_) = old_https_proxy_lower {
        std::env::set_var("https_proxy", old.unwrap());
    } else {
        std::env::remove_var("https_proxy");
    }

    result
}

/// Compare two version strings (e.g., "1.2.3" vs "1.2.4")
/// Returns: 1 if v1 > v2, -1 if v1 < v2, 0 if equal
fn compare_versions(v1: &str, v2: &str) -> i32 {
    let parts1: Vec<i32> = v1.split('.').filter_map(|s| s.parse().ok()).collect();
    let parts2: Vec<i32> = v2.split('.').filter_map(|s| s.parse().ok()).collect();

    let max_len = parts1.len().max(parts2.len());

    for i in 0..max_len {
        let num1 = parts1.get(i).copied().unwrap_or(0);
        let num2 = parts2.get(i).copied().unwrap_or(0);

        if num1 > num2 {
            return 1;
        }
        if num1 < num2 {
            return -1;
        }
    }

    0
}

#[cfg(test)]
mod tests {
    use super::is_scoop_install_path;

    #[test]
    fn detects_per_user_scoop_install() {
        assert!(is_scoop_install_path(
            "C:\\Users\\foo\\scoop\\apps\\ai-toolbox\\1.1.4\\ai-toolbox.exe",
            &[],
        ));
    }

    #[test]
    fn detects_global_and_verbatim_paths_case_insensitively() {
        assert!(is_scoop_install_path(
            "C:\\Program Files\\Scoop\\apps\\ai-toolbox\\1.1.4\\ai-toolbox.exe",
            &[],
        ));
        assert!(is_scoop_install_path(
            "\\\\?\\D:\\Scoop\\Apps\\ai-toolbox\\1.1.4\\ai-toolbox.exe",
            &[],
        ));
    }

    #[test]
    fn rejects_non_scoop_paths() {
        assert!(!is_scoop_install_path(
            "C:\\Program Files\\AI Toolbox\\ai-toolbox.exe",
            &[],
        ));
        assert!(!is_scoop_install_path(
            "C:\\Users\\foo\\AppData\\Local\\AI Toolbox\\ai-toolbox.exe",
            &[],
        ));
        // "scoopless\apps" must not match "\scoop\apps\"
        assert!(!is_scoop_install_path(
            "C:\\Users\\foo\\scoopless\\apps\\ai-toolbox\\ai-toolbox.exe",
            &[],
        ));
    }

    #[test]
    fn detects_custom_scoop_roots_without_matching_adjacent_directories() {
        let roots = vec![r"D:\Tools\".to_string(), "E:/Global Packages".to_string()];
        assert!(is_scoop_install_path(
            r"\\?\D:\Tools\apps\ai-toolbox\current\ai-toolbox.exe",
            &roots
        ));
        assert!(is_scoop_install_path(
            "E:/Global Packages/apps/ai-toolbox/1.1.4/ai-toolbox.exe",
            &roots
        ));
        assert!(!is_scoop_install_path(
            r"D:\Tools-old\apps\ai-toolbox\ai-toolbox.exe",
            &roots
        ));
        assert!(!is_scoop_install_path(
            r"D:\Tools\apps-backup\ai-toolbox.exe",
            &roots
        ));
        assert!(is_scoop_install_path(
            r"\\?\UNC\server\tools\apps\ai-toolbox\current\ai-toolbox.exe",
            &[r"\\server\tools".to_string()],
        ));
    }
}
