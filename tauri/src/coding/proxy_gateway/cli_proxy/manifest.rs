use crate::coding::proxy_gateway::{
    aggregate_naming::AggregateNamingMode,
    types::{GatewayCliKey, GatewayProxyMode},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

/// Maximum length accepted for a strict aggregate group id.
pub const AGGREGATE_GROUP_ID_MAX_LEN: usize = 32;

/// A strict aggregate group and the proxyable providers assigned to it.
///
/// Groups are intentionally kept as a manifest-level contract. Runtime
/// routing may consume them separately, while an empty list preserves the
/// historical aggregate behavior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AggregateGroup {
    pub id: String,
    #[serde(default)]
    pub provider_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AggregateManifestConfig {
    /// Sites selected for aggregate routing, in the user's display order.
    #[serde(default)]
    pub provider_ids: Vec<String>,
    /// Separator between the effective site prefix and upstream model name.
    /// Defaults to `.`.
    #[serde(default = "default_aggregate_separator")]
    pub separator: String,
    /// Per-site display/routing prefixes. New legacy aggregate manifests persist
    /// a safe provider display name here when no explicit alias was supplied;
    /// old manifests with no entry still fall back to the provider id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub aliases: BTreeMap<String, String>,
    /// How `(site, model)` pairs are named in the generated Codex catalog.
    #[serde(default)]
    pub naming: AggregateNamingMode,
    /// Optional strict provider groups. An empty list keeps legacy aggregate
    /// routing semantics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<AggregateGroup>,
}

fn default_aggregate_separator() -> String {
    AGGREGATE_DEFAULT_SEPARATOR.to_string()
}

impl Default for AggregateManifestConfig {
    fn default() -> Self {
        Self {
            provider_ids: Vec::new(),
            separator: default_aggregate_separator(),
            aliases: BTreeMap::new(),
            naming: AggregateNamingMode::default(),
            groups: Vec::new(),
        }
    }
}

/// Default separator between the site id and the upstream model name in
/// aggregate mode. `.` keeps the generated slugs acceptable to Codex's
/// telemetry tags (unlike `:`) while still being readable.
pub const AGGREGATE_DEFAULT_SEPARATOR: &str = ".";

/// Validate a user-supplied aggregate separator.
///
/// The separator must be non-empty and must not contain characters that are
/// legal inside a site prefix, otherwise `<site><sep><model>` becomes
/// ambiguous and cannot be split back reliably.
pub fn validate_aggregate_separator(separator: &str) -> Result<(), String> {
    if separator.is_empty() {
        return Err("Aggregate separator must not be empty".to_string());
    }
    if separator
        .chars()
        .any(|ch| ch.is_alphanumeric() || ch.is_whitespace() || ch == '_' || ch == '-')
    {
        return Err(
            "Aggregate separator must not contain letters, digits, whitespace, '_' or '-'"
                .to_string(),
        );
    }
    Ok(())
}

/// Validate one strict aggregate group id.
pub fn validate_aggregate_group_id(group_id: &str) -> Result<(), String> {
    if group_id.is_empty() {
        return Err("Aggregate group id must not be empty".to_string());
    }
    if group_id.chars().count() > AGGREGATE_GROUP_ID_MAX_LEN {
        return Err(format!(
            "Aggregate group id must be at most {AGGREGATE_GROUP_ID_MAX_LEN} characters"
        ));
    }
    if group_id
        .chars()
        .any(|ch| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'))
    {
        return Err("Aggregate group id may only contain letters, digits, '_' or '-'".to_string());
    }
    Ok(())
}

/// Validate strict aggregate groups against the currently proxyable providers.
///
/// Provider ids may intentionally appear in more than one group. Duplicates
/// within one group are rejected because they do not define a meaningful
/// priority order.
pub fn validate_aggregate_groups(
    groups: &[AggregateGroup],
    proxyable_provider_ids: &[String],
) -> Result<(), String> {
    let proxyable = proxyable_provider_ids.iter().collect::<BTreeSet<_>>();
    let mut seen_group_ids = BTreeSet::new();

    for group in groups {
        validate_aggregate_group_id(&group.id)
            .map_err(|error| format!("Aggregate group '{}': {error}", group.id))?;

        let normalized_group_id = group.id.to_ascii_lowercase();
        if !seen_group_ids.insert(normalized_group_id) {
            return Err(format!(
                "Aggregate group id '{}' is used more than once; group ids must be unique case-insensitively",
                group.id
            ));
        }

        if group.provider_ids.is_empty() {
            return Err(format!(
                "Aggregate group '{}' must contain at least one provider",
                group.id
            ));
        }

        let mut seen_provider_ids = BTreeSet::new();
        for provider_id in &group.provider_ids {
            if !proxyable.contains(provider_id) {
                return Err(format!(
                    "Provider '{provider_id}' in aggregate group '{}' is not available for Gateway proxy",
                    group.id
                ));
            }
            if !seen_provider_ids.insert(provider_id) {
                return Err(format!(
                    "Provider '{provider_id}' appears more than once in aggregate group '{}'",
                    group.id
                ));
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CliProxyManifest {
    pub schema_version: u32,
    pub managed_by: String,
    pub cli_key: GatewayCliKey,
    pub enabled: bool,
    pub mode: GatewayProxyMode,
    pub primary_provider_id: String,
    pub base_origin: String,
    pub created_at: String,
    pub updated_at: String,
    pub files: Vec<CliProxyManifestFile>,
    /// Aggregate-mode routing config. Absent for single/failover manifests, and
    /// absent in manifests written before aggregate mode existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate: Option<AggregateManifestConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CliProxyManifestFile {
    pub kind: String,
    pub path: String,
    pub existed: bool,
    pub backup_rel_path: String,
    pub backup_sha256: Option<String>,
    pub backup_size: Option<u64>,
    pub managed_fields: Vec<String>,
}

impl CliProxyManifest {
    pub fn new(
        cli_key: GatewayCliKey,
        base_origin: String,
        timestamp: String,
        mode: GatewayProxyMode,
        primary_provider_id: String,
    ) -> Self {
        Self {
            schema_version: 1,
            managed_by: "ai-toolbox-proxy-gateway".to_string(),
            cli_key,
            enabled: true,
            mode,
            primary_provider_id,
            base_origin,
            created_at: timestamp.clone(),
            updated_at: timestamp,
            files: Vec::new(),
            aggregate: None,
        }
    }

    /// Attach aggregate routing config and switch the manifest to aggregate mode.
    pub fn with_aggregate(
        mut self,
        provider_ids: Vec<String>,
        separator: String,
        aliases: BTreeMap<String, String>,
        naming: AggregateNamingMode,
    ) -> Self {
        self = self.with_aggregate_groups(provider_ids, separator, aliases, naming, Vec::new());
        self
    }

    /// Attach aggregate routing config with optional strict groups and switch
    /// the manifest to aggregate mode.
    pub fn with_aggregate_groups(
        mut self,
        provider_ids: Vec<String>,
        separator: String,
        aliases: BTreeMap<String, String>,
        naming: AggregateNamingMode,
        groups: Vec<AggregateGroup>,
    ) -> Self {
        self.mode = GatewayProxyMode::Aggregate;
        self.aggregate = Some(AggregateManifestConfig {
            provider_ids,
            separator,
            aliases,
            naming,
            groups,
        });
        self
    }
}

pub fn validate_backup_rel_path(path: &str) -> Result<(), String> {
    if path.contains(':') || path.contains('\\') {
        return Err("Manifest backup path must use a relative forward-slash path".to_string());
    }
    let path = Path::new(path);
    if path.is_absolute() {
        return Err("Manifest backup path must be relative".to_string());
    }
    for component in path.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err("Manifest backup path cannot escape the backup directory".to_string())
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_serializes_without_provider_data() {
        let mut manifest = CliProxyManifest::new(
            GatewayCliKey::Codex,
            "http://127.0.0.1:37123".to_string(),
            "2026-05-16T10:00:00Z".to_string(),
            GatewayProxyMode::Single,
            "provider-1".to_string(),
        );
        manifest.files.push(CliProxyManifestFile {
            kind: "codex_config_toml".to_string(),
            path: "C:\\Users\\User\\.codex\\config.toml".to_string(),
            existed: true,
            backup_rel_path: "backups/config.toml".to_string(),
            backup_sha256: Some("abc".to_string()),
            backup_size: Some(123),
            managed_fields: vec![
                "model_providers.custom.base_url".to_string(),
                "model_providers.custom.wire_api".to_string(),
                "model_providers.custom.experimental_bearer_token".to_string(),
            ],
        });

        let json = serde_json::to_string(&manifest).unwrap();

        assert!(json.contains("codex_config_toml"));
        assert!(json.contains("primary_provider_id"));
        assert!(!json.contains("settings_config"));
        assert!(!json.contains("api_key"));
    }

    #[test]
    fn backup_relative_path_accepts_normal_path() {
        assert!(validate_backup_rel_path("backups/config.toml").is_ok());
    }

    #[test]
    fn backup_relative_path_rejects_parent_escape() {
        assert!(validate_backup_rel_path("../config.toml").is_err());
        assert!(validate_backup_rel_path("backups/../../config.toml").is_err());
    }

    #[test]
    fn backup_relative_path_rejects_absolute_path() {
        assert!(validate_backup_rel_path("C:\\Users\\config.toml").is_err());
        assert!(validate_backup_rel_path("/tmp/config.toml").is_err());
    }

    #[test]
    fn aggregate_manifest_defaults_naming_for_older_manifests() {
        let parsed: AggregateManifestConfig = serde_json::from_value(serde_json::json!({
            "provider_ids": ["site-a"],
            "separator": "."
        }))
        .unwrap();

        assert!(parsed.aliases.is_empty());
        assert_eq!(parsed.naming, AggregateNamingMode::SiteModel);
        assert!(parsed.groups.is_empty());
    }

    #[test]
    fn aggregate_group_id_validation_enforces_charset_and_length() {
        assert!(validate_aggregate_group_id("group-1").is_ok());
        assert!(validate_aggregate_group_id("group_1").is_ok());
        assert!(validate_aggregate_group_id("").is_err());
        assert!(validate_aggregate_group_id("group 1").is_err());
        assert!(validate_aggregate_group_id("group.1").is_err());
        assert!(validate_aggregate_group_id(&"a".repeat(AGGREGATE_GROUP_ID_MAX_LEN + 1)).is_err());
    }

    #[test]
    fn aggregate_groups_validate_provider_members_and_allow_cross_group_membership() {
        let available = vec!["provider-a".to_string(), "provider-b".to_string()];
        let groups = vec![
            AggregateGroup {
                id: "fast".to_string(),
                provider_ids: vec!["provider-a".to_string()],
            },
            AggregateGroup {
                id: "slow".to_string(),
                provider_ids: vec!["provider-a".to_string(), "provider-b".to_string()],
            },
        ];

        assert!(validate_aggregate_groups(&groups, &available).is_ok());
    }

    #[test]
    fn aggregate_groups_reject_duplicate_ids_empty_groups_and_unknown_providers() {
        let available = vec!["provider-a".to_string()];
        assert!(validate_aggregate_groups(
            &[
                AggregateGroup {
                    id: "Fast".to_string(),
                    provider_ids: vec!["provider-a".to_string()],
                },
                AggregateGroup {
                    id: "fast".to_string(),
                    provider_ids: vec!["provider-a".to_string()],
                },
            ],
            &available,
        )
        .is_err());
        assert!(validate_aggregate_groups(
            &[AggregateGroup {
                id: "empty".to_string(),
                provider_ids: Vec::new(),
            }],
            &available,
        )
        .is_err());
        assert!(validate_aggregate_groups(
            &[AggregateGroup {
                id: "unknown".to_string(),
                provider_ids: vec!["provider-b".to_string()],
            }],
            &available,
        )
        .is_err());
    }
}
