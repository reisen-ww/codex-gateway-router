use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tauri::Emitter;
use walkdir::WalkDir;

use crate::coding::runtime_location;
use crate::db::SqliteDbState;

// New OpenCode Markdown Agents use the canonical plural `agents/` directory.
// Keep reading and editing the legacy singular `agent/` directory so existing
// user files do not disappear after upgrade. Scan the legacy directory first so
// a same-name canonical agent remains the effective source in the UI.
const AGENT_DIRECTORY_NAMES: [&str; 2] = ["agent", "agents"];
const BUILT_IN_AGENT_NAMES: [&str; 8] = [
    "build",
    "plan",
    "general",
    "explore",
    "scout",
    "title",
    "summary",
    "compaction",
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeMarkdownAgent {
    pub name: String,
    pub path: String,
    pub directory: String,
    pub frontmatter: String,
    pub prompt: String,
    pub raw_content: String,
    pub content_hash: String,
    pub config: Option<Value>,
    pub parse_error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveOpenCodeMarkdownAgentRequest {
    pub path: String,
    pub expected_content_hash: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteOpenCodeMarkdownAgentRequest {
    pub path: String,
    pub expected_content_hash: String,
}

fn content_hash(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

fn local_home_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME"));
    #[cfg(not(target_os = "windows"))]
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"));

    home.map(PathBuf::from)
        .map_err(|_| "Failed to determine the current user home directory".to_string())
}

fn expand_local_home(path: &str) -> Result<PathBuf, String> {
    let trimmed = path.trim();
    if trimmed == "~" {
        return local_home_dir();
    }
    if let Some(relative) = trimmed
        .strip_prefix("~/")
        .or_else(|| trimmed.strip_prefix("~\\"))
    {
        return Ok(local_home_dir()?.join(relative));
    }
    Ok(PathBuf::from(trimmed))
}

fn global_config_dir_from_runtime(
    location: &runtime_location::RuntimeLocationInfo,
) -> Result<PathBuf, String> {
    if let Some(wsl) = &location.wsl {
        let linux_user_root = wsl.linux_user_root.as_deref().ok_or_else(|| {
            "Failed to determine the WSL user home for OpenCode Agent files".to_string()
        })?;
        let linux_path = format!("{}/.config/opencode", linux_user_root.trim_end_matches('/'));
        return Ok(runtime_location::build_windows_unc_path(
            &wsl.distro,
            &linux_path,
        ));
    }

    Ok(local_home_dir()?.join(".config").join("opencode"))
}

async fn markdown_agent_config_dirs(db: &SqliteDbState) -> Result<Vec<PathBuf>, String> {
    let location = runtime_location::get_opencode_runtime_location_async(db).await?;
    let mut directories = vec![global_config_dir_from_runtime(&location)?];

    if location.wsl.is_none() {
        let configured_dir = std::env::var("OPENCODE_CONFIG_DIR")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                super::shell_env::get_env_from_shell_config("OPENCODE_CONFIG_DIR")
                    .filter(|value| !value.trim().is_empty())
            });
        if let Some(configured_dir) = configured_dir {
            directories.push(expand_local_home(&configured_dir)?);
        }
    }

    let mut seen = HashSet::new();
    directories.retain(|directory| seen.insert(directory.clone()));
    Ok(directories)
}

fn parse_markdown_agent(content: &str) -> Result<(String, String, Value), String> {
    let normalized = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut offset = 0usize;
    let mut lines = normalized.split_inclusive('\n');
    let first_line = lines
        .next()
        .ok_or_else(|| "Agent file is empty".to_string())?;
    if first_line.trim_end_matches(['\r', '\n']) != "---" {
        return Err("Agent file must start with YAML frontmatter delimited by ---".to_string());
    }
    offset += first_line.len();
    let frontmatter_start = offset;
    let mut frontmatter_end = None;
    let mut body_start = None;

    for line in lines {
        let line_start = offset;
        offset += line.len();
        let marker = line.trim_end_matches(['\r', '\n']);
        if marker == "---" || marker == "..." {
            frontmatter_end = Some(line_start);
            body_start = Some(offset);
            break;
        }
    }

    let frontmatter_end = frontmatter_end
        .ok_or_else(|| "Agent YAML frontmatter is missing a closing --- delimiter".to_string())?;
    let body_start = body_start.unwrap_or(normalized.len());
    let frontmatter = normalized[frontmatter_start..frontmatter_end]
        .trim_end_matches(['\r', '\n'])
        .to_string();
    let prompt = normalized[body_start..]
        .trim_start_matches(['\r', '\n'])
        .to_string();
    let yaml_value = serde_yaml::from_str::<serde_yaml::Value>(&frontmatter)
        .map_err(|error| format!("Failed to parse YAML frontmatter: {error}"))?;
    let config = serde_json::to_value(yaml_value)
        .map_err(|error| format!("Failed to convert YAML frontmatter: {error}"))?;
    if !config.is_object() {
        return Err("Agent YAML frontmatter must be an object".to_string());
    }

    Ok((frontmatter, prompt, config))
}

fn validate_markdown_agent_config(name: &str, config: &Value) -> Result<(), String> {
    let object = config
        .as_object()
        .ok_or_else(|| "Agent YAML frontmatter must be an object".to_string())?;
    if !BUILT_IN_AGENT_NAMES.contains(&name) {
        let description = object
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if description.is_empty() {
            return Err(
                "Custom OpenCode Markdown Agents require a non-empty description".to_string(),
            );
        }
    }
    if object.get("model").is_some_and(|value| !value.is_string()) {
        return Err("OpenCode Agent model must be a string".to_string());
    }
    if object
        .get("variant")
        .is_some_and(|value| !value.is_string())
    {
        return Err("OpenCode Agent variant must be a string".to_string());
    }
    if let Some(mode) = object.get("mode") {
        let valid = matches!(mode.as_str(), Some("primary" | "subagent" | "all"));
        if !valid {
            return Err("OpenCode Agent mode must be primary, subagent, or all".to_string());
        }
    }
    Ok(())
}

fn agent_name(root: &Path, file_path: &Path) -> Option<String> {
    let relative = file_path.strip_prefix(root).ok()?;
    let mut components = relative.components();
    let first = components.next()?.as_os_str().to_string_lossy();
    if !AGENT_DIRECTORY_NAMES.contains(&first.as_ref()) {
        return None;
    }

    let remaining = components.as_path();
    let mut name = remaining.to_string_lossy().replace('\\', "/");
    if !name.to_ascii_lowercase().ends_with(".md") {
        return None;
    }
    name.truncate(name.len() - 3);
    (!name.is_empty()).then_some(name)
}

fn read_markdown_agent(root: &Path, file_path: &Path) -> Result<OpenCodeMarkdownAgent, String> {
    let raw_content = fs::read_to_string(file_path)
        .map_err(|error| format!("Failed to read {}: {error}", file_path.display()))?;
    let name = agent_name(root, file_path)
        .ok_or_else(|| format!("Invalid OpenCode Agent path: {}", file_path.display()))?;
    let (frontmatter, prompt, config, parse_error) = match parse_markdown_agent(&raw_content) {
        Ok((frontmatter, prompt, config)) => match validate_markdown_agent_config(&name, &config) {
            Ok(()) => (frontmatter, prompt, Some(config), None),
            Err(error) => (frontmatter, prompt, Some(config), Some(error)),
        },
        Err(error) => (String::new(), String::new(), None, Some(error)),
    };

    Ok(OpenCodeMarkdownAgent {
        name,
        path: file_path.to_string_lossy().to_string(),
        directory: root.to_string_lossy().to_string(),
        frontmatter,
        prompt,
        raw_content: raw_content.clone(),
        content_hash: content_hash(&raw_content),
        config,
        parse_error,
    })
}

fn ensure_safe_agent_path(path: &Path, roots: &[PathBuf]) -> Result<(), String> {
    let is_markdown = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"));
    if !is_markdown {
        return Err("OpenCode Agent files must use the .md extension".to_string());
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("OpenCode Agent path cannot contain parent directory segments".to_string());
    }

    let allowed = roots.iter().any(|root| {
        AGENT_DIRECTORY_NAMES
            .iter()
            .any(|directory_name| path.starts_with(root.join(directory_name)))
    });
    if !allowed {
        return Err(
            "The selected file is outside the configured OpenCode Agent directories".to_string(),
        );
    }
    Ok(())
}

async fn validate_requested_path(
    db: &SqliteDbState,
    requested_path: &str,
) -> Result<PathBuf, String> {
    let path = PathBuf::from(requested_path);
    let roots = markdown_agent_config_dirs(db).await?;
    ensure_safe_agent_path(&path, &roots)?;
    Ok(path)
}

#[tauri::command]
pub async fn list_opencode_markdown_agents(
    state: tauri::State<'_, SqliteDbState>,
) -> Result<Vec<OpenCodeMarkdownAgent>, String> {
    let roots = markdown_agent_config_dirs(state.db()).await?;
    let mut agents = Vec::new();

    for root in roots {
        for directory_name in AGENT_DIRECTORY_NAMES {
            let directory = root.join(directory_name);
            if !directory.exists() {
                continue;
            }
            let mut files = WalkDir::new(&directory)
                .follow_links(false)
                .into_iter()
                .filter_map(|entry| match entry {
                    Ok(entry) => Some(entry),
                    Err(error) => {
                        log::warn!(
                            "Failed to inspect OpenCode Markdown Agent entry under {}: {error}",
                            directory.display()
                        );
                        None
                    }
                })
                .filter(|entry| entry.file_type().is_file())
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
                })
                .map(|entry| entry.into_path())
                .collect::<Vec<_>>();
            files.sort();
            for file in files {
                match read_markdown_agent(&root, &file) {
                    Ok(agent) => agents.push(agent),
                    Err(error) => log::warn!("{error}"),
                }
            }
        }
    }

    Ok(agents)
}

#[tauri::command]
pub async fn save_opencode_markdown_agent<R: tauri::Runtime>(
    state: tauri::State<'_, SqliteDbState>,
    app: tauri::AppHandle<R>,
    request: SaveOpenCodeMarkdownAgentRequest,
) -> Result<OpenCodeMarkdownAgent, String> {
    let path = validate_requested_path(state.db(), &request.path).await?;
    let roots = markdown_agent_config_dirs(state.db()).await?;
    let root = roots
        .iter()
        .find(|root| {
            AGENT_DIRECTORY_NAMES
                .iter()
                .any(|name| path.starts_with(root.join(name)))
        })
        .ok_or_else(|| "Failed to resolve the OpenCode Agent directory".to_string())?;
    let name = agent_name(root, &path)
        .ok_or_else(|| format!("Invalid OpenCode Agent path: {}", path.display()))?;
    let (_, _, config) = parse_markdown_agent(&request.content)?;
    validate_markdown_agent_config(&name, &config)?;
    let current_content = fs::read_to_string(&path)
        .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
    if content_hash(&current_content) != request.expected_content_hash {
        return Err(
            "OpenCode Agent file changed outside AI Toolbox Gateway Router. Reload before saving."
                .to_string(),
        );
    }

    fs::write(&path, request.content)
        .map_err(|error| format!("Failed to write {}: {error}", path.display()))?;
    let saved = read_markdown_agent(root, &path)?;

    let _ = app.emit("config-changed", "window");
    #[cfg(target_os = "windows")]
    let _ = app.emit("wsl-sync-request-opencode", ());
    Ok(saved)
}

#[tauri::command]
pub async fn delete_opencode_markdown_agent<R: tauri::Runtime>(
    state: tauri::State<'_, SqliteDbState>,
    app: tauri::AppHandle<R>,
    request: DeleteOpenCodeMarkdownAgentRequest,
) -> Result<(), String> {
    let path = validate_requested_path(state.db(), &request.path).await?;
    let current_content = fs::read_to_string(&path)
        .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
    if content_hash(&current_content) != request.expected_content_hash {
        return Err(
            "OpenCode Agent file changed outside AI Toolbox Gateway Router. Reload before deleting."
                .to_string(),
        );
    }
    fs::remove_file(&path)
        .map_err(|error| format!("Failed to delete {}: {error}", path.display()))?;

    let _ = app.emit("config-changed", "window");
    #[cfg(target_os = "windows")]
    let _ = app.emit("wsl-sync-request-opencode", ());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        agent_name, ensure_safe_agent_path, global_config_dir_from_runtime, parse_markdown_agent,
        AGENT_DIRECTORY_NAMES,
    };
    use crate::coding::runtime_location::{
        RuntimeLocationInfo, RuntimeLocationMode, WslLocationInfo,
    };
    use std::path::Path;

    #[test]
    fn parses_markdown_agent_frontmatter_and_prompt() {
        let content = "---\ndescription: Reviews code\nmode: subagent\n---\n\nReview carefully.\n";
        let (frontmatter, prompt, config) = parse_markdown_agent(content).unwrap();
        assert!(frontmatter.contains("description: Reviews code"));
        assert_eq!(prompt, "Review carefully.\n");
        assert_eq!(config["mode"], "subagent");
    }

    #[test]
    fn keeps_nested_agent_names() {
        let root = Path::new("/home/test/.config/opencode");
        let file = root.join("agents/review/security.md");
        assert_eq!(agent_name(root, &file).as_deref(), Some("review/security"));
    }

    #[test]
    fn rejects_paths_outside_agent_directories() {
        let roots = vec![Path::new("/home/test/.config/opencode").to_path_buf()];
        assert!(ensure_safe_agent_path(
            Path::new("/home/test/.config/opencode/agents/review.md"),
            &roots,
        )
        .is_ok());
        assert!(ensure_safe_agent_path(
            Path::new("/home/test/.config/opencode/prompts/review.md"),
            &roots,
        )
        .is_err());
    }

    #[test]
    fn canonical_plural_directory_has_higher_precedence() {
        assert_eq!(AGENT_DIRECTORY_NAMES, ["agent", "agents"]);
    }

    #[test]
    fn accepts_legacy_singular_agent_directory() {
        let root = Path::new("/home/test/.config/opencode");
        let roots = vec![root.to_path_buf()];
        let legacy_file = root.join("agent/review.md");

        assert!(ensure_safe_agent_path(&legacy_file, &roots).is_ok());
        assert_eq!(agent_name(root, &legacy_file).as_deref(), Some("review"));
    }

    #[test]
    fn wsl_custom_config_path_does_not_become_markdown_agent_directory() {
        let location = RuntimeLocationInfo {
            mode: RuntimeLocationMode::WslDirect,
            source: "custom".to_string(),
            host_path: Path::new(
                r"\\wsl.localhost\Ubuntu\home\tester\custom\opencode.custom.jsonc",
            )
            .to_path_buf(),
            wsl: Some(WslLocationInfo {
                distro: "Ubuntu".to_string(),
                linux_path: "/home/tester/custom/opencode.custom.jsonc".to_string(),
                linux_user_root: Some("/home/tester".to_string()),
            }),
        };

        assert_eq!(
            global_config_dir_from_runtime(&location).unwrap(),
            Path::new(r"\\wsl.localhost\Ubuntu\home\tester\.config\opencode")
        );
    }
}
