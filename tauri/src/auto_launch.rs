//! Auto Launch Module
//!
//! Provides cross-platform auto-start functionality using the auto-launch crate.
//! - Windows: Registry (HKCU\Software\Microsoft\Windows\CurrentVersion\Run)
//! - macOS: LaunchAgent or AppleScript Login Item
//! - Linux: XDG autostart (~/.config/autostart/)

use thiserror::Error;

#[derive(Error, Debug)]
pub enum AutoLaunchError {
    #[error("Failed to get executable path: {0}")]
    ExePath(String),
    #[error("Failed to build auto launch: {0}")]
    Build(String),
    #[error("Failed to enable auto launch: {0}")]
    Enable(String),
    #[error("Failed to disable auto launch: {0}")]
    Disable(String),
    #[error("Failed to check auto launch status: {0}")]
    Check(String),
}

/// macOS: Get .app bundle path from executable path
/// Converts `/path/to/AI Toolbox Gateway Router.app/Contents/MacOS/AI Toolbox Gateway Router`
/// to `/path/to/AI Toolbox Gateway Router.app`
#[cfg(target_os = "macos")]
fn get_macos_app_bundle_path(exe_path: &std::path::Path) -> Option<std::path::PathBuf> {
    let path_str = exe_path.to_string_lossy();
    // Find .app/Contents/MacOS/ pattern
    if let Some(app_pos) = path_str.find(".app/Contents/MacOS/") {
        let app_bundle_end = app_pos + 4; // End of ".app"
        Some(std::path::PathBuf::from(&path_str[..app_bundle_end]))
    } else {
        None
    }
}

/// Initialize AutoLaunch instance
fn get_auto_launch() -> Result<auto_launch::AutoLaunch, AutoLaunchError> {
    use auto_launch::AutoLaunchBuilder;

    let app_name = "AI Toolbox Gateway Router";
    let exe_path = std::env::current_exe().map_err(|e| AutoLaunchError::ExePath(e.to_string()))?;

    // macOS needs .app bundle path, otherwise AppleScript login item will open terminal
    #[cfg(target_os = "macos")]
    let app_path = get_macos_app_bundle_path(&exe_path).unwrap_or(exe_path);

    #[cfg(not(target_os = "macos"))]
    let app_path = exe_path;

    // Use AutoLaunchBuilder to eliminate platform differences
    // macOS: Uses AppleScript method (default), requires .app bundle path
    // Windows/Linux: Uses Registry/XDG autostart
    AutoLaunchBuilder::new()
        .set_app_name(app_name)
        .set_app_path(&app_path.to_string_lossy())
        .build()
        .map_err(|e| AutoLaunchError::Build(e.to_string()))
}

/// Enable auto launch on startup
pub fn enable_auto_launch() -> Result<(), AutoLaunchError> {
    let auto_launch = get_auto_launch()?;
    auto_launch
        .enable()
        .map_err(|e| AutoLaunchError::Enable(e.to_string()))?;
    Ok(())
}

/// Disable auto launch on startup
pub fn disable_auto_launch() -> Result<(), AutoLaunchError> {
    let auto_launch = get_auto_launch()?;
    auto_launch
        .disable()
        .map_err(|e| AutoLaunchError::Disable(e.to_string()))?;
    Ok(())
}

/// Check if auto launch is enabled
pub fn is_auto_launch_enabled() -> Result<bool, AutoLaunchError> {
    let auto_launch = get_auto_launch()?;
    auto_launch
        .is_enabled()
        .map_err(|e| AutoLaunchError::Check(e.to_string()))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::get_macos_app_bundle_path;

    #[test]
    fn test_get_macos_app_bundle_path_valid() {
        let exe_path =
            std::path::Path::new("/Applications/AI Toolbox.app/Contents/MacOS/AI Toolbox");
        let result = get_macos_app_bundle_path(exe_path);
        assert_eq!(
            result,
            Some(std::path::PathBuf::from("/Applications/AI Toolbox.app"))
        );
    }

    #[test]
    fn test_get_macos_app_bundle_path_with_spaces() {
        let exe_path =
            std::path::Path::new("/Users/test/My Apps/AI Toolbox.app/Contents/MacOS/AI Toolbox");
        let result = get_macos_app_bundle_path(exe_path);
        assert_eq!(
            result,
            Some(std::path::PathBuf::from(
                "/Users/test/My Apps/AI Toolbox.app"
            ))
        );
    }

    #[test]
    fn test_get_macos_app_bundle_path_not_in_bundle() {
        let exe_path = std::path::Path::new("/usr/local/bin/ai-toolbox");
        let result = get_macos_app_bundle_path(exe_path);
        assert_eq!(result, None);
    }
}
