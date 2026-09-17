use crate::coding::proxy_gateway::types::{
    normalize_pricing_model_source, CodexChatReasoningMeta, CustomHeaderOverride,
    GatewayAggregateGroup, GatewayCliKey, GatewayProviderProfileReference, GatewayProxyMode,
    ModelRewriteRule, ProviderGatewayMeta, ProviderPriorityEntry, ProxyGatewaySettings,
};
use crate::coding::proxy_gateway::{
    aggregate_naming::{
        build_aggregate_slug_table, split_model_at_site_slug, split_site_model_slug,
        AggregateNamingConfig, AggregateNamingMode,
    },
    cli_proxy::manifest::CliProxyManifest,
    paths::ProxyGatewayPaths,
    provider_profiles::load_gateway_provider_profiles_for_runtime,
    transformer::AiProtocol,
};
use crate::coding::{claude_code, claude_desktop, codex, gemini_cli, grok, kimi};
use crate::db::helpers::db_list;
use crate::db::schema::{DbTable, OrderDirection, OrderField, OrderSpec};
use crate::db::SqliteDbState;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const SELECTION_MANIFEST_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct SelectionManifestCacheEntry {
    loaded_at: Instant,
    selection: Option<GatewayProviderSelection>,
}

fn selection_manifest_cache() -> &'static Mutex<HashMap<String, SelectionManifestCacheEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, SelectionManifestCacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpstreamProvider {
    pub(crate) cli_key: GatewayCliKey,
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) base_url: String,
    pub(crate) api_key: String,
    pub(crate) target_protocol: AiProtocol,
    pub(crate) supports_websockets: Option<bool>,
    pub(crate) auth_strategy: ProviderAuthStrategy,
    pub(crate) is_full_url: bool,
    pub(crate) sort_index: Option<i32>,
    pub(crate) meta: ProviderGatewayMeta,
    pub(crate) model_mapping: UpstreamModelMapping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderAuthStrategy {
    Bearer,
    AnthropicApiKey,
    GoogleApiKey,
    GoogleOAuth,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct UpstreamModelMapping {
    pub(crate) default_model: Option<String>,
    /// Codex provider-level auto-review model override from
    /// `settingsConfig.autoReviewModelOverride`. Used on failover guardian
    /// requests so each candidate channel owns its own review model.
    pub(crate) auto_review_model: Option<String>,
    pub(crate) haiku_model: Option<String>,
    pub(crate) sonnet_model: Option<String>,
    pub(crate) opus_model: Option<String>,
    pub(crate) fable_model: Option<String>,
    pub(crate) reasoning_model: Option<String>,
    /// User-defined exact rewrite rules from provider `data.meta.modelRewrites`
    /// (issue #321). Applied before every per-CLI mapping in every proxy mode.
    pub(crate) rewrite_rules: Vec<ModelRewriteRule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GatewayProviderSelection {
    pub(crate) mode: GatewayProxyMode,
    pub(crate) primary_provider_id: String,
    /// Aggregate mode only: the sites the user selected, in display order.
    /// Empty in single/failover mode, and ignored there.
    pub(crate) aggregate_provider_ids: Vec<String>,
    /// Aggregate mode only: the separator between site id and model name.
    pub(crate) aggregate_separator: String,
    /// Aggregate mode only: user aliases for selected sites.
    pub(crate) aggregate_aliases: std::collections::BTreeMap<String, String>,
    /// Aggregate mode only: template used by the Codex catalog and router.
    pub(crate) aggregate_naming: AggregateNamingMode,
    /// Aggregate mode only: strict groups. An empty list preserves the legacy
    /// aggregate routing behavior.
    pub(crate) aggregate_groups: Vec<GatewayAggregateGroup>,
}

pub(crate) async fn load_candidate_providers(
    db: &SqliteDbState,
    cli_key: GatewayCliKey,
) -> Result<Vec<UpstreamProvider>, String> {
    load_candidate_providers_with_settings(db, cli_key, None).await
}

pub(crate) async fn load_candidate_providers_with_settings(
    db: &SqliteDbState,
    cli_key: GatewayCliKey,
    settings: Option<&ProxyGatewaySettings>,
) -> Result<Vec<UpstreamProvider>, String> {
    load_candidate_providers_with_settings_and_selection(db, cli_key, settings, None).await
}

pub(crate) async fn load_candidate_providers_with_settings_and_selection(
    db: &SqliteDbState,
    cli_key: GatewayCliKey,
    settings: Option<&ProxyGatewaySettings>,
    selection: Option<&GatewayProviderSelection>,
) -> Result<Vec<UpstreamProvider>, String> {
    let table = match cli_key {
        GatewayCliKey::Claude => DbTable::ClaudeProvider,
        GatewayCliKey::ClaudeDesktop => DbTable::ClaudeDesktopProvider,
        GatewayCliKey::Codex => DbTable::CodexProvider,
        GatewayCliKey::Grok => DbTable::GrokProvider,
        GatewayCliKey::Kimi => DbTable::KimiProvider,
        GatewayCliKey::Gemini => DbTable::GeminiCliProvider,
        GatewayCliKey::OpenCode => {
            return Err(
                "OpenCode adapter is intentionally out of scope for the gateway MVP".to_string(),
            )
        }
    };
    let order = OrderSpec::new(vec![
        OrderField::json_integer("sort_index", OrderDirection::Asc)?,
        OrderField::json_text("updated_at", OrderDirection::Desc)?,
    ]);
    let records = db.with_conn(|conn| db_list(conn, table, Some(&order)))?;

    let strict_aggregate = selection.is_some_and(|selection| {
        selection.mode == GatewayProxyMode::Aggregate && !selection.aggregate_groups.is_empty()
    });
    let mut providers = Vec::new();
    let mut parse_errors = Vec::new();
    for record in records {
        match provider_from_record_with_catalog_mode(cli_key, record, settings, strict_aggregate) {
            Ok(Some(provider)) => providers.push(provider),
            Ok(None) => {}
            Err(error) => parse_errors.push(error),
        }
    }
    sort_candidate_providers(&mut providers);

    if providers.is_empty() && !parse_errors.is_empty() {
        return Err(parse_errors.join("; "));
    }

    apply_provider_selection(providers, selection)
}

pub(crate) async fn load_provider_by_id_for_connectivity_test(
    db: &SqliteDbState,
    cli_key: GatewayCliKey,
    provider_id: &str,
    settings: Option<&ProxyGatewaySettings>,
) -> Result<UpstreamProvider, String> {
    let table = match cli_key {
        GatewayCliKey::Claude => DbTable::ClaudeProvider,
        GatewayCliKey::ClaudeDesktop => DbTable::ClaudeDesktopProvider,
        GatewayCliKey::Codex => DbTable::CodexProvider,
        GatewayCliKey::Grok => DbTable::GrokProvider,
        GatewayCliKey::Kimi => DbTable::KimiProvider,
        GatewayCliKey::Gemini => DbTable::GeminiCliProvider,
        GatewayCliKey::OpenCode => {
            return Err(
                "OpenCode adapter is intentionally out of scope for the gateway MVP".to_string(),
            )
        }
    };
    let records = db.with_conn(|conn| db_list(conn, table, None))?;
    let record = records
        .into_iter()
        .find(|record| record.get("id").and_then(Value::as_str) == Some(provider_id))
        .ok_or_else(|| {
            format!(
                "Provider '{provider_id}' was not found for {}",
                cli_key.as_str()
            )
        })?;
    provider_from_record(cli_key, record, settings)?.ok_or_else(|| {
        format!(
            "Provider '{provider_id}' is disabled or not eligible for Gateway connectivity testing"
        )
    })
}

/// Sync helper kept for non-async call sites/tests. Request hot path uses the async variant.
#[allow(dead_code)]
pub(crate) fn load_gateway_provider_selection(
    paths: Option<&ProxyGatewayPaths>,
    cli_key: GatewayCliKey,
) -> Result<Option<GatewayProviderSelection>, String> {
    let Some(paths) = paths else {
        return Ok(None);
    };
    let manifest_path = paths.manifest_path(cli_key);
    let cache_key = format!("{}:{}", cli_key.as_str(), manifest_path.display());
    if let Ok(cache) = selection_manifest_cache().lock() {
        if let Some(entry) = cache.get(&cache_key) {
            if entry.loaded_at.elapsed() < SELECTION_MANIFEST_CACHE_TTL {
                return Ok(entry.selection.clone());
            }
        }
    }
    let selection = if !manifest_path.exists() {
        None
    } else {
        let content = fs::read_to_string(&manifest_path).map_err(|error| {
            format!(
                "Failed to read gateway manifest {}: {}",
                manifest_path.display(),
                error
            )
        })?;
        let manifest = serde_json::from_str::<CliProxyManifest>(&content).map_err(|error| {
            format!(
                "Failed to parse gateway manifest {}: {}",
                manifest_path.display(),
                error
            )
        })?;
        if !manifest.enabled {
            None
        } else {
            let aggregate = manifest.aggregate.clone().unwrap_or_default();
            Some(GatewayProviderSelection {
                mode: manifest.mode,
                primary_provider_id: manifest.primary_provider_id,
                aggregate_provider_ids: aggregate.provider_ids,
                aggregate_separator: aggregate.separator,
                aggregate_aliases: aggregate.aliases,
                aggregate_naming: aggregate.naming,
                aggregate_groups: aggregate
                    .groups
                    .into_iter()
                    .map(|group| GatewayAggregateGroup {
                        id: group.id,
                        provider_ids: group.provider_ids,
                    })
                    .collect(),
            })
        }
    };
    if let Ok(mut cache) = selection_manifest_cache().lock() {
        cache.insert(
            cache_key,
            SelectionManifestCacheEntry {
                loaded_at: Instant::now(),
                selection: selection.clone(),
            },
        );
    }
    Ok(selection)
}

/// Async variant for request hot paths: avoids blocking a Tokio worker on disk I/O.
pub(crate) async fn load_gateway_provider_selection_async(
    paths: Option<&ProxyGatewayPaths>,
    cli_key: GatewayCliKey,
) -> Result<Option<GatewayProviderSelection>, String> {
    let Some(paths) = paths else {
        return Ok(None);
    };
    let manifest_path = paths.manifest_path(cli_key);
    let cache_key = format!("{}:{}", cli_key.as_str(), manifest_path.display());
    if let Ok(cache) = selection_manifest_cache().lock() {
        if let Some(entry) = cache.get(&cache_key) {
            if entry.loaded_at.elapsed() < SELECTION_MANIFEST_CACHE_TTL {
                return Ok(entry.selection.clone());
            }
        }
    }
    let selection = if !tokio::fs::try_exists(&manifest_path)
        .await
        .map_err(|error| {
            format!(
                "Failed to check gateway manifest {}: {}",
                manifest_path.display(),
                error
            )
        })? {
        None
    } else {
        let content = tokio::fs::read_to_string(&manifest_path)
            .await
            .map_err(|error| {
                format!(
                    "Failed to read gateway manifest {}: {}",
                    manifest_path.display(),
                    error
                )
            })?;
        let manifest = serde_json::from_str::<CliProxyManifest>(&content).map_err(|error| {
            format!(
                "Failed to parse gateway manifest {}: {}",
                manifest_path.display(),
                error
            )
        })?;
        if !manifest.enabled {
            None
        } else {
            let aggregate = manifest.aggregate.clone().unwrap_or_default();
            Some(GatewayProviderSelection {
                mode: manifest.mode,
                primary_provider_id: manifest.primary_provider_id,
                aggregate_provider_ids: aggregate.provider_ids,
                aggregate_separator: aggregate.separator,
                aggregate_aliases: aggregate.aliases,
                aggregate_naming: aggregate.naming,
                aggregate_groups: aggregate
                    .groups
                    .into_iter()
                    .map(|group| GatewayAggregateGroup {
                        id: group.id,
                        provider_ids: group.provider_ids,
                    })
                    .collect(),
            })
        }
    };
    if let Ok(mut cache) = selection_manifest_cache().lock() {
        cache.insert(
            cache_key,
            SelectionManifestCacheEntry {
                loaded_at: Instant::now(),
                selection: selection.clone(),
            },
        );
    }
    Ok(selection)
}

/// Drop cached selection manifests after engage/disengage so the next request
/// re-reads disk instead of waiting for the 30s TTL.
pub(crate) fn clear_gateway_provider_selection_cache() {
    if let Ok(mut cache) = selection_manifest_cache().lock() {
        cache.clear();
    }
}

pub(crate) fn provider_priority_entries(
    providers: &[UpstreamProvider],
) -> Vec<ProviderPriorityEntry> {
    providers
        .iter()
        .enumerate()
        .map(|(index, provider)| ProviderPriorityEntry {
            provider_id: provider.id.clone(),
            label: format!("P{index}"),
        })
        .collect()
}

fn apply_provider_selection(
    mut providers: Vec<UpstreamProvider>,
    selection: Option<&GatewayProviderSelection>,
) -> Result<Vec<UpstreamProvider>, String> {
    let Some(selection) = selection else {
        return Ok(providers);
    };

    let primary_index = providers
        .iter()
        .position(|provider| provider.id == selection.primary_provider_id);
    match selection.mode {
        GatewayProxyMode::Single => {
            let Some(primary_index) = primary_index else {
                return Err(format!(
                    "Primary gateway proxy provider '{}' was not found for {}",
                    selection.primary_provider_id,
                    selection.mode.as_str()
                ));
            };
            Ok(vec![providers.remove(primary_index)])
        }
        GatewayProxyMode::Failover => {
            if let Some(primary_index) = primary_index {
                let primary_provider = providers.remove(primary_index);
                let mut ordered_providers = Vec::with_capacity(providers.len() + 1);
                ordered_providers.push(primary_provider);
                ordered_providers.extend(providers);
                Ok(ordered_providers)
            } else {
                Ok(providers)
            }
        }
        // Legacy aggregate mode keeps every candidate so the request-time model
        // prefix can pick the target. Selected sites are promoted to the front
        // in the order the user arranged them; unselected sites stay available
        // as fallbacks.
        GatewayProxyMode::Aggregate => {
            if !selection.aggregate_groups.is_empty() {
                // Strict groups are the authoritative provider selection. Do
                // not let a stale or hand-written top-level provider_ids list
                // add, remove, or reorder the providers available to a group.
                let manifest_groups = selection
                    .aggregate_groups
                    .iter()
                    .map(|group| {
                        crate::coding::proxy_gateway::cli_proxy::manifest::AggregateGroup {
                            id: group.id.clone(),
                            provider_ids: group.provider_ids.clone(),
                        }
                    })
                    .collect::<Vec<_>>();
                crate::coding::proxy_gateway::cli_proxy::manifest::validate_aggregate_groups(
                    &manifest_groups,
                    &providers
                        .iter()
                        .map(|provider| provider.id.clone())
                        .collect::<Vec<_>>(),
                )?;

                let mut ordered = Vec::new();
                for group in &selection.aggregate_groups {
                    for provider_id in &group.provider_ids {
                        if let Some(index) = providers
                            .iter()
                            .position(|provider| &provider.id == provider_id)
                        {
                            ordered.push(providers.remove(index));
                        }
                    }
                }
                return Ok(ordered);
            }
            let selected = &selection.aggregate_provider_ids;
            if selected.is_empty() {
                return Ok(providers);
            }
            let mut ordered = Vec::with_capacity(providers.len());
            for id in selected {
                if let Some(index) = providers.iter().position(|provider| &provider.id == id) {
                    ordered.push(providers.remove(index));
                }
            }
            ordered.extend(providers);
            Ok(ordered)
        }
    }
}

/// The resolved target of one aggregate-mode request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AggregateRoute {
    /// Site id parsed from the model prefix, when the request carried one.
    pub(crate) site_id: Option<String>,
    /// Strict aggregate group id parsed from the canonical `group.model`
    /// prefix. A route has either a site id (legacy aggregate mode) or a
    /// group id (strict aggregate mode), never both.
    pub(crate) group_id: Option<String>,
    /// Model name to forward upstream (prefix stripped).
    pub(crate) upstream_model: String,
    /// `true` when the exact model slug selected a site and must bypass model
    /// rewrite/default mapping.
    pub(crate) explicit: bool,
}

/// Resolve which site should serve an aggregate-mode request.
///
/// Order of resolution:
/// 1. Explicit `<site_id><sep><model>` prefix naming a known site.
/// 2. First candidate whose declared model catalog contains the requested
///    model name (mirrors the non-aggregate "bare model name" behaviour).
///
/// `providers` must already be ordered by preference.
#[cfg(test)]
pub(crate) fn resolve_aggregate_route(
    requested_model: &str,
    separator: &str,
    providers: &[UpstreamProvider],
) -> AggregateRoute {
    let selection = GatewayProviderSelection {
        mode: GatewayProxyMode::Aggregate,
        primary_provider_id: providers
            .first()
            .map(|provider| provider.id.clone())
            .unwrap_or_default(),
        aggregate_provider_ids: providers
            .iter()
            .map(|provider| provider.id.clone())
            .collect(),
        aggregate_separator: separator.to_string(),
        aggregate_aliases: std::collections::BTreeMap::new(),
        aggregate_naming: AggregateNamingMode::SiteModel,
        aggregate_groups: Vec::new(),
    };
    resolve_aggregate_route_with_selection(requested_model, &selection, providers).unwrap_or_else(
        |_| AggregateRoute {
            site_id: None,
            group_id: None,
            upstream_model: requested_model.to_string(),
            explicit: false,
        },
    )
}

pub(crate) fn resolve_aggregate_route_with_selection(
    requested_model: &str,
    selection: &GatewayProviderSelection,
    providers: &[UpstreamProvider],
) -> Result<AggregateRoute, String> {
    if !selection.aggregate_groups.is_empty() {
        // Strict groups deliberately use one stable model namespace regardless
        // of the legacy aggregate naming template/separator: `<group>.model`.
        // This prevents a request from selecting a site, a different group, or
        // the ungrouped aggregate fallback path.
        let parsed = split_site_model_slug(
            requested_model,
            ".",
            selection
                .aggregate_groups
                .iter()
                .map(|group| (group.id.as_str(), group.id.as_str())),
        );
        let Some((group_id, upstream_model)) = parsed else {
            return Err(format!(
                "Strict aggregate mode requires a model name in the form \
                 '<group>.model'; '{}' does not name a configured group",
                requested_model
            ));
        };
        return Ok(AggregateRoute {
            site_id: None,
            group_id: Some(group_id),
            upstream_model,
            explicit: true,
        });
    }

    let selected_sites = selection
        .aggregate_provider_ids
        .iter()
        .filter_map(|site_id| {
            providers
                .iter()
                .find(|provider| &provider.id == site_id)
                .map(|provider| (provider.id.clone(), provider.meta.declared_models.clone()))
        })
        .collect::<Vec<_>>();
    let naming = AggregateNamingConfig {
        separator: selection.aggregate_separator.clone(),
        aliases: selection.aggregate_aliases.clone(),
        naming: selection.aggregate_naming,
    };
    let table = build_aggregate_slug_table(
        &selected_sites,
        &naming.separator,
        &naming.aliases,
        naming.naming,
    )?;
    if let Some(entry) = table.iter().find(|entry| entry.slug == requested_model) {
        return Ok(AggregateRoute {
            site_id: Some(entry.site_id.clone()),
            group_id: None,
            upstream_model: entry.upstream_model.clone(),
            explicit: true,
        });
    }

    // Preserve the historical provider-id prefix contract even after a user
    // opts into aliases/display names or a different catalog template. This
    // also keeps every enabled (unselected) candidate addressable in aggregate
    // mode.
    let parsed = match naming.naming {
        AggregateNamingMode::SiteModel => split_site_model_slug(
            requested_model,
            &naming.separator,
            providers.iter().map(|provider| {
                (
                    provider.id.as_str(),
                    crate::coding::proxy_gateway::aggregate_naming::aggregate_site_prefix(
                        provider.id.as_str(),
                        &naming.aliases,
                    ),
                )
            }),
        ),
        AggregateNamingMode::ModelAtSite => split_model_at_site_slug(
            requested_model,
            &naming.separator,
            providers.iter().map(|provider| {
                (
                    provider.id.as_str(),
                    crate::coding::proxy_gateway::aggregate_naming::aggregate_site_prefix(
                        provider.id.as_str(),
                        &naming.aliases,
                    ),
                )
            }),
        ),
        AggregateNamingMode::ModelOnly => None,
    }
    .or_else(|| match naming.naming {
        AggregateNamingMode::SiteModel => split_site_model_slug(
            requested_model,
            &naming.separator,
            providers
                .iter()
                .map(|provider| (provider.id.as_str(), provider.id.as_str())),
        ),
        AggregateNamingMode::ModelAtSite => split_model_at_site_slug(
            requested_model,
            &naming.separator,
            providers
                .iter()
                .map(|provider| (provider.id.as_str(), provider.id.as_str())),
        ),
        AggregateNamingMode::ModelOnly => None,
    });
    if let Some((site_id, upstream_model)) = parsed {
        return Ok(AggregateRoute {
            site_id: Some(site_id),
            group_id: None,
            upstream_model,
            explicit: true,
        });
    }

    Ok(AggregateRoute {
        site_id: None,
        group_id: None,
        upstream_model: requested_model.to_string(),
        explicit: false,
    })
}

fn sort_candidate_providers(providers: &mut [UpstreamProvider]) {
    providers.sort_by(|left, right| {
        left.sort_index
            .unwrap_or(0)
            .cmp(&right.sort_index.unwrap_or(0))
            .then_with(|| left.name.cmp(&right.name))
    });
}

fn provider_from_record(
    cli_key: GatewayCliKey,
    record: Value,
    settings: Option<&ProxyGatewaySettings>,
) -> Result<Option<UpstreamProvider>, String> {
    provider_from_record_with_catalog_mode(cli_key, record, settings, false)
}

fn provider_from_record_with_catalog_mode(
    cli_key: GatewayCliKey,
    record: Value,
    settings: Option<&ProxyGatewaySettings>,
    strict_aggregate: bool,
) -> Result<Option<UpstreamProvider>, String> {
    let meta =
        provider_meta_from_record_with_catalog_mode(cli_key, &record, settings, strict_aggregate);
    match cli_key {
        GatewayCliKey::Claude => {
            let provider = claude_code::adapter::from_db_value_provider(record);
            if provider.is_disabled {
                return Ok(None);
            }
            if is_official_provider_category(&provider.category) {
                return Ok(None);
            }
            let settings =
                parse_json_config(&provider.settings_config, "Claude provider settings_config")?;
            let env = settings.get("env").and_then(Value::as_object);
            let target_protocol = claude_target_protocol(&meta, &settings);
            let (api_key, mut auth_strategy) = claude_auth_from_settings(
                env,
                &settings,
                target_protocol,
                meta.api_key_field.as_deref(),
                &provider.name,
            )?;
            if target_protocol == AiProtocol::AnthropicMessages
                && anthropic_platform_uses_bearer_auth(&meta)
            {
                auth_strategy = ProviderAuthStrategy::Bearer;
            }
            let base_url = json_object_string(env, "ANTHROPIC_BASE_URL")
                .unwrap_or_else(|| "https://api.anthropic.com".to_string());
            let (base_url, is_full_url) = normalize_provider_base_url(base_url, meta.is_full_url);
            let model_mapping = UpstreamModelMapping {
                rewrite_rules: model_rewrite_rules_from_meta(&meta),
                ..claude_model_mapping_from_settings(&settings)
            };
            Ok(Some(UpstreamProvider {
                supports_websockets: None,
                cli_key,
                id: provider.id,
                name: provider.name,
                base_url,
                api_key,
                target_protocol,
                auth_strategy,
                is_full_url,
                sort_index: provider.sort_index,
                meta,
                model_mapping,
            }))
        }
        GatewayCliKey::Codex => {
            let provider = codex::adapter::from_db_value_provider(record);
            if provider.is_disabled {
                return Ok(None);
            }
            if is_official_provider_category(&provider.category) {
                return Ok(None);
            }
            let settings =
                parse_json_config(&provider.settings_config, "Codex provider settings_config")?;
            let auth = settings.get("auth").and_then(Value::as_object);
            let api_key = json_object_string(auth, "OPENAI_API_KEY").ok_or_else(|| {
                format!("Codex provider '{}' has no OPENAI_API_KEY", provider.name)
            })?;
            let config_toml = settings.get("config").and_then(Value::as_str).unwrap_or("");
            let base_url = codex_base_url_from_config(config_toml)
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            let (base_url, is_full_url) = normalize_provider_base_url(base_url, meta.is_full_url);
            let target_protocol = codex_target_protocol(&meta, &settings, &base_url);
            let mut auth_strategy = auth_strategy_for_target_protocol(
                target_protocol,
                meta.api_key_field.as_deref(),
                &api_key,
                ProviderAuthStrategy::Bearer,
            );
            if target_protocol == AiProtocol::AnthropicMessages
                && anthropic_platform_uses_bearer_auth(&meta)
            {
                auth_strategy = ProviderAuthStrategy::Bearer;
            }
            Ok(Some(UpstreamProvider {
                supports_websockets:
                    super::super::provider_protocol::codex_supports_websockets_from_config(
                        config_toml,
                    ),
                cli_key,
                id: provider.id,
                name: provider.name,
                base_url,
                api_key,
                target_protocol,
                auth_strategy,
                is_full_url,
                sort_index: provider.sort_index,
                model_mapping: UpstreamModelMapping {
                    default_model: codex_model_from_config(config_toml),
                    rewrite_rules: model_rewrite_rules_from_meta(&meta),
                    auto_review_model: codex_auto_review_model_from_settings(&settings).or_else(
                        || {
                            settings
                                .get("modelCatalog")
                                .and_then(codex_auto_review_model_from_catalog)
                        },
                    ),
                    ..UpstreamModelMapping::default()
                },
                meta,
            }))
        }
        GatewayCliKey::Grok => {
            let provider = grok::adapter::provider_from_db_value(record);
            if provider.is_disabled || is_official_provider_category(&provider.category) {
                return Ok(None);
            }
            let settings =
                parse_json_config(&provider.settings_config, "Grok provider settings_config")?;
            let config_toml = settings.get("config").and_then(Value::as_str).unwrap_or("");
            let selected_model = grok_selected_model_from_settings(&settings, config_toml);
            let api_key = settings
                .pointer("/auth/API_KEY")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    selected_model
                        .as_ref()
                        .and_then(|model| model.api_key.clone())
                })
                .ok_or_else(|| format!("Grok provider '{}' has no API_KEY", provider.name))?;
            let base_url = selected_model
                .as_ref()
                .and_then(|model| model.base_url.clone())
                .unwrap_or_else(|| "https://api.x.ai/v1".to_string());
            let (base_url, is_full_url) = normalize_provider_base_url(base_url, meta.is_full_url);
            let target_protocol = meta
                .api_format
                .as_deref()
                .and_then(AiProtocol::from_api_format)
                .or_else(|| {
                    selected_model
                        .as_ref()
                        .and_then(|model| model.api_backend.as_deref())
                        .and_then(AiProtocol::from_api_format)
                })
                .unwrap_or(AiProtocol::OpenAiChat);
            let mut auth_strategy = auth_strategy_for_target_protocol(
                target_protocol,
                meta.api_key_field.as_deref(),
                &api_key,
                ProviderAuthStrategy::Bearer,
            );
            if target_protocol == AiProtocol::AnthropicMessages
                && anthropic_platform_uses_bearer_auth(&meta)
            {
                auth_strategy = ProviderAuthStrategy::Bearer;
            }
            Ok(Some(UpstreamProvider {
                supports_websockets: None,
                cli_key,
                id: provider.id,
                name: provider.name,
                base_url,
                api_key,
                target_protocol,
                auth_strategy,
                is_full_url,
                sort_index: provider.sort_index,
                model_mapping: UpstreamModelMapping {
                    default_model: selected_model.and_then(|model| model.model_id),
                    rewrite_rules: model_rewrite_rules_from_meta(&meta),
                    ..UpstreamModelMapping::default()
                },
                meta,
            }))
        }
        GatewayCliKey::Kimi => {
            let provider = kimi::adapter::provider_from_db_value(record);
            if provider.is_disabled || is_official_provider_category(&provider.category) {
                return Ok(None);
            }
            let settings =
                parse_json_config(&provider.settings_config, "Kimi provider settings_config")?;
            let (selected_provider, selected_key) = kimi_selected_provider_entry(&settings)
                .map(|(config, key)| (Some(config), key))
                .unwrap_or((None, None));
            if selected_key.is_none() {
                let config_count = settings
                    .get("providerConfigs")
                    .and_then(Value::as_object)
                    .map(|configs| configs.len())
                    .unwrap_or(0);
                if config_count > 1 {
                    log::warn!(
                        "[proxy-gateway] Kimi provider '{}' has an incomplete defaultModelKey -> modelCatalog chain; the gateway candidate falls back to the first providerConfigs entry while the takeover patch targets the managed:kimi-code table",
                        provider.id
                    );
                }
            }
            // The Kimi CLI allows per-provider keys in `[providers.<key>].api_key`,
            // which survives adoption as `providerConfigs.<key>.api_key`; records
            // without any resolvable key (e.g. official OAuth setups adopted as
            // custom) are skipped like official instead of failing the whole
            // candidate list and turning the CLI takeover status into an error.
            let api_key = settings
                .pointer("/auth/API_KEY")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    selected_provider
                        .and_then(|config| config.get("api_key"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                });
            let Some(api_key) = api_key else {
                return Ok(None);
            };
            let base_url = selected_provider
                .and_then(|config| config.get("base_url"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(kimi::constants::KIMI_OFFICIAL_API_BASE_URL)
                .to_string();
            let (base_url, is_full_url) = normalize_provider_base_url(base_url, meta.is_full_url);
            let target_protocol = meta
                .api_format
                .as_deref()
                .and_then(AiProtocol::from_api_format)
                .unwrap_or(AiProtocol::OpenAiChat);
            let mut auth_strategy = auth_strategy_for_target_protocol(
                target_protocol,
                meta.api_key_field.as_deref(),
                &api_key,
                ProviderAuthStrategy::Bearer,
            );
            if target_protocol == AiProtocol::AnthropicMessages
                && anthropic_platform_uses_bearer_auth(&meta)
            {
                auth_strategy = ProviderAuthStrategy::Bearer;
            }
            Ok(Some(UpstreamProvider {
                supports_websockets: None,
                cli_key,
                id: provider.id,
                name: provider.name,
                base_url,
                api_key,
                target_protocol,
                auth_strategy,
                is_full_url,
                sort_index: provider.sort_index,
                model_mapping: UpstreamModelMapping {
                    default_model: settings
                        .get("defaultModelKey")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string),
                    rewrite_rules: model_rewrite_rules_from_meta(&meta),
                    ..UpstreamModelMapping::default()
                },
                meta,
            }))
        }
        GatewayCliKey::Gemini => {
            let provider = gemini_cli::adapter::from_db_value_provider(record);
            if provider.is_disabled {
                return Ok(None);
            }
            if is_official_provider_category(&provider.category) {
                return Ok(None);
            }
            let settings = parse_json_config(
                &provider.settings_config,
                "Gemini CLI provider settings_config",
            )?;
            let env = settings.get("env").and_then(Value::as_object);
            let base_url = json_object_string(env, "GOOGLE_GEMINI_BASE_URL")
                .or_else(|| json_object_string(env, "GOOGLE_VERTEX_BASE_URL"))
                .unwrap_or_else(|| "https://generativelanguage.googleapis.com/v1beta".to_string());
            let (base_url, is_full_url) = normalize_provider_base_url(base_url, meta.is_full_url);
            let target_protocol = gemini_target_protocol(&meta, &settings);
            let (api_key, mut auth_strategy) = gemini_auth_from_settings(
                env,
                &settings,
                target_protocol,
                meta.api_key_field.as_deref(),
                &provider.name,
            )?;
            if target_protocol == AiProtocol::AnthropicMessages
                && anthropic_platform_uses_bearer_auth(&meta)
            {
                auth_strategy = ProviderAuthStrategy::Bearer;
            }
            Ok(Some(UpstreamProvider {
                supports_websockets: None,
                cli_key,
                id: provider.id,
                name: provider.name,
                base_url,
                api_key,
                target_protocol,
                auth_strategy,
                is_full_url,
                sort_index: provider.sort_index,
                model_mapping: UpstreamModelMapping {
                    rewrite_rules: model_rewrite_rules_from_meta(&meta),
                    ..UpstreamModelMapping::default()
                },
                meta,
            }))
        }
        // Claude Desktop reuses the Claude Code settings shape: its 3P profile
        // points at the local gateway while the DB record still holds the real
        // upstream channel config, so the same env-headedness applies here.
        GatewayCliKey::ClaudeDesktop => {
            let provider = claude_desktop::adapter::from_db_value_provider(record);
            if provider.is_disabled {
                return Ok(None);
            }
            if is_official_provider_category(&provider.category) {
                return Ok(None);
            }
            let settings = parse_json_config(
                &provider.settings_config,
                "Claude Desktop provider settings_config",
            )?;
            let env = settings.get("env").and_then(Value::as_object);
            let target_protocol = claude_target_protocol(&meta, &settings);
            let (api_key, mut auth_strategy) = claude_auth_from_settings(
                env,
                &settings,
                target_protocol,
                meta.api_key_field.as_deref(),
                &provider.name,
            )?;
            if target_protocol == AiProtocol::AnthropicMessages
                && anthropic_platform_uses_bearer_auth(&meta)
            {
                auth_strategy = ProviderAuthStrategy::Bearer;
            }
            let base_url = json_object_string(env, "ANTHROPIC_BASE_URL")
                .unwrap_or_else(|| "https://api.anthropic.com".to_string());
            let (base_url, is_full_url) = normalize_provider_base_url(base_url, meta.is_full_url);
            let model_mapping = UpstreamModelMapping {
                rewrite_rules: model_rewrite_rules_from_meta(&meta),
                ..claude_desktop_model_mapping(&provider.meta, &settings)
            };
            Ok(Some(UpstreamProvider {
                supports_websockets: None,
                cli_key,
                id: provider.id,
                name: provider.name,
                base_url,
                api_key,
                target_protocol,
                auth_strategy,
                is_full_url,
                sort_index: provider.sort_index,
                meta,
                model_mapping,
            }))
        }
        GatewayCliKey::OpenCode => unreachable!("OpenCode is rejected before query"),
    }
}

fn is_official_provider_category(category: &str) -> bool {
    category.trim().eq_ignore_ascii_case("official")
}

fn normalize_provider_base_url(base_url: String, is_full_url: bool) -> (String, bool) {
    if let Some(raw_url) = base_url.trim().strip_suffix("##") {
        return (raw_url.trim_end().to_string(), true);
    }
    (base_url, is_full_url)
}

fn anthropic_platform_uses_bearer_auth(meta: &ProviderGatewayMeta) -> bool {
    meta.provider_type
        .as_deref()
        .map(|value| value.trim().to_ascii_lowercase().replace(['_', ' '], "-"))
        .is_some_and(|value| {
            matches!(
                value.as_str(),
                "bedrock" | "anthropic-bedrock" | "aws-bedrock" | "longcat" | "long-cat"
            )
        })
}

#[cfg(test)]
fn provider_meta_from_record(
    cli_key: GatewayCliKey,
    record: &Value,
    settings: Option<&ProxyGatewaySettings>,
) -> ProviderGatewayMeta {
    provider_meta_from_record_with_catalog_mode(cli_key, record, settings, false)
}

fn provider_meta_from_record_with_catalog_mode(
    cli_key: GatewayCliKey,
    record: &Value,
    settings: Option<&ProxyGatewaySettings>,
    strict_aggregate: bool,
) -> ProviderGatewayMeta {
    let meta_value = record.get("meta").unwrap_or(&Value::Null);
    let default_cost_multiplier = settings
        .map(|settings| settings.default_cost_multiplier_for(cli_key))
        .unwrap_or_else(|| "1.0".to_string());
    let default_pricing_model_source = settings
        .map(|settings| settings.default_pricing_model_source_for(cli_key))
        .unwrap_or_else(|| "upstream".to_string());
    let mut meta = ProviderGatewayMeta {
        gateway_profile: gateway_profile_reference_from_meta(meta_value),
        provider_type: json_string_compat(meta_value, "provider_type", "providerType"),
        api_format: json_string_compat(meta_value, "api_format", "apiFormat"),
        api_key_field: json_string_compat(meta_value, "api_key_field", "apiKeyField"),
        is_full_url: json_bool_compat(meta_value, "is_full_url", "isFullUrl").unwrap_or(false),
        prompt_cache_key: json_string_compat(meta_value, "prompt_cache_key", "promptCacheKey"),
        reasoning_field: json_string_compat(meta_value, "reasoning_field", "reasoningField"),
        default_max_tokens: json_i64_compat(meta_value, "default_max_tokens", "defaultMaxTokens"),
        codex_chat_reasoning: meta_value
            .get("codex_chat_reasoning")
            .or_else(|| meta_value.get("codexChatReasoning"))
            .and_then(|value| serde_json::from_value(value.clone()).ok()),
        image_input_policy: json_string_compat(
            meta_value,
            "image_input_policy",
            "imageInputPolicy",
        ),
        text_only_models: json_string_vec_compat(meta_value, "text_only_models", "textOnlyModels"),
        image_capable_models: json_string_vec_compat(
            meta_value,
            "image_capable_models",
            "imageCapableModels",
        ),
        allow_text_only_model_heuristic: json_bool_compat(
            meta_value,
            "allow_text_only_model_heuristic",
            "allowTextOnlyModelHeuristic",
        )
        .unwrap_or(false),
        cost_multiplier: json_string_compat(meta_value, "cost_multiplier", "costMultiplier")
            .unwrap_or_else(|| default_cost_multiplier.clone()),
        pricing_model_source: json_string_compat(
            meta_value,
            "pricing_model_source",
            "pricingModelSource",
        )
        .map(|value| normalize_pricing_model_source(&value))
        .unwrap_or_else(|| default_pricing_model_source.clone()),
        custom_headers: custom_headers_from_meta(meta_value),
        model_rewrites: model_rewrites_from_meta(meta_value),
        declared_models: Vec::new(),
    };
    apply_gateway_profile_reference(cli_key, &mut meta);
    if meta.provider_type.is_none() {
        meta.provider_type = record
            .get("category")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
    }
    if meta.cost_multiplier.trim().is_empty() {
        meta.cost_multiplier = default_cost_multiplier;
    }
    if meta.pricing_model_source.trim().is_empty() {
        meta.pricing_model_source = default_pricing_model_source;
    }
    merge_model_catalog_image_capabilities(&mut meta, record.get("settings_config"));
    meta.declared_models =
        declared_models_from_settings(record.get("settings_config"), strict_aggregate);
    meta
}

/// Read the upstream model ids a provider declares in its model settings.
///
/// Legacy aggregate mode intentionally retains its historical precedence:
/// `modelCatalog.models` is authoritative when present, otherwise root
/// `models` is used. Strict groups opt into the expanded source order so the
/// runtime matches the strict Codex catalog (`modelCatalog.models`, root
/// `models`, then the provider default model from config.toml).
fn declared_models_from_settings(
    settings_config: Option<&Value>,
    strict_aggregate: bool,
) -> Vec<String> {
    let Some(settings_config) = settings_config else {
        return Vec::new();
    };
    let settings_value = match settings_config {
        Value::String(text) => serde_json::from_str::<Value>(text).ok(),
        Value::Object(_) => Some(settings_config.clone()),
        _ => None,
    };
    let Some(value) = settings_value.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let catalog_models = value
        .get("modelCatalog")
        .and_then(|catalog| catalog.get("models"))
        .and_then(Value::as_array);
    let root_models = value.get("models").and_then(Value::as_array);

    if let Some(models) = catalog_models {
        for model in models {
            if let Some(model_id) = declared_model_id_from_settings_item(model, strict_aggregate) {
                push_unique_string(&mut out, model_id);
            }
        }
    } else if let Some(models) = root_models {
        // This preserves the old fallback when modelCatalog is absent or
        // malformed, while an explicitly empty catalog remains authoritative.
        for model in models {
            if let Some(model_id) = declared_model_id_from_settings_item(model, strict_aggregate) {
                push_unique_string(&mut out, model_id);
            }
        }
    }

    if strict_aggregate {
        if catalog_models.is_some() {
            if let Some(models) = root_models {
                for model in models {
                    if let Some(model_id) =
                        declared_model_id_from_settings_item(model, strict_aggregate)
                    {
                        push_unique_string(&mut out, model_id);
                    }
                }
            }
        }
        if let Some(config_toml) = value.get("config").and_then(Value::as_str) {
            if let Some(model_id) = codex_model_from_config(config_toml) {
                push_unique_string(&mut out, model_id);
            }
        }
    }
    out
}

/// Strict aggregate catalogs accept string model shorthands in addition to the
/// object rows supported by the legacy provider metadata path. Keep the
/// legacy path unchanged while making runtime model membership match the
/// strict Codex catalog exactly.
fn declared_model_id_from_settings_item(model: &Value, strict_aggregate: bool) -> Option<String> {
    if strict_aggregate {
        if let Some(model_id) = model
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
        {
            return Some(model_id);
        }
    }
    model_catalog_model_id(model)
}

fn gateway_profile_reference_from_meta(value: &Value) -> Option<GatewayProviderProfileReference> {
    let reference = value
        .get("gateway_profile")
        .or_else(|| value.get("gatewayProfile"))?;
    let profile_id = json_string_compat(reference, "profile_id", "profileId")?;
    let endpoint_id = json_string_compat(reference, "endpoint_id", "endpointId")?;
    Some(GatewayProviderProfileReference {
        tool: json_value_string(reference, "tool"),
        profile_id,
        endpoint_id,
    })
}

fn apply_gateway_profile_reference(cli_key: GatewayCliKey, meta: &mut ProviderGatewayMeta) {
    let Some(reference) = meta.gateway_profile.clone() else {
        return;
    };
    let Some(tool) = gateway_profile_tool_for_cli(cli_key) else {
        return;
    };
    if !gateway_profile_reference_matches_tool(&reference, tool) {
        return;
    }
    let Some(catalog) = load_gateway_provider_profiles_for_runtime() else {
        return;
    };
    let Some((profile, endpoint)) = find_gateway_profile_endpoint(
        &catalog,
        tool,
        reference.profile_id.trim(),
        reference.endpoint_id.trim(),
    ) else {
        return;
    };

    meta.provider_type = json_value_string(profile, "providerType");
    meta.api_format = json_value_string(endpoint, "apiFormat");
    meta.api_key_field = json_value_string(endpoint, "apiKeyField")
        .or_else(|| json_value_string(profile, "apiKeyField"));
    meta.reasoning_field = json_value_string(endpoint, "reasoningField")
        .or_else(|| json_value_string(profile, "reasoningField"));
    meta.default_max_tokens = json_i64_compat(endpoint, "default_max_tokens", "defaultMaxTokens")
        .or_else(|| json_i64_compat(profile, "default_max_tokens", "defaultMaxTokens"));
    meta.image_input_policy = json_value_string(endpoint, "imageInputPolicy")
        .or_else(|| json_value_string(profile, "imageInputPolicy"));
    meta.text_only_models = {
        let endpoint_models =
            json_string_vec_compat(endpoint, "text_only_models", "textOnlyModels");
        if endpoint_models.is_empty() {
            json_string_vec_compat(profile, "text_only_models", "textOnlyModels")
        } else {
            endpoint_models
        }
    };
    meta.image_capable_models = {
        let endpoint_models =
            json_string_vec_compat(endpoint, "image_capable_models", "imageCapableModels");
        if endpoint_models.is_empty() {
            json_string_vec_compat(profile, "image_capable_models", "imageCapableModels")
        } else {
            endpoint_models
        }
    };
    meta.allow_text_only_model_heuristic = json_bool_compat(
        endpoint,
        "allow_text_only_model_heuristic",
        "allowTextOnlyModelHeuristic",
    )
    .or_else(|| {
        json_bool_compat(
            profile,
            "allow_text_only_model_heuristic",
            "allowTextOnlyModelHeuristic",
        )
    })
    .unwrap_or(false);
    meta.codex_chat_reasoning = if tool == "codex" {
        codex_chat_reasoning_from_profile_value(endpoint)
            .or_else(|| codex_chat_reasoning_from_profile_value(profile))
    } else {
        None
    };
}

/// Resolve the providerConfigs entry a gateway candidate should mirror.
///
/// Mirrors the CLI's own lookup chain used by the takeover patch
/// (`resolve_kimi_takeover_provider_key`): `defaultModelKey` ->
/// `modelCatalog.models[].provider` -> `providerConfigs[provider]`. Picking
/// the first `providerConfigs` entry blindly would point gateway traffic at
/// the wrong upstream when a record carries multiple providers. The returned
/// key is `None` when the chain is incomplete and the selection fell back to
/// the first entry — the takeover patch resolves the same incomplete chain to
/// the managed official key instead, so with multiple entries candidates and
/// the patch target may diverge for hand-edited records (both still route
/// through the gateway); callers log that case.
fn kimi_selected_provider_entry(settings: &Value) -> Option<(&Value, Option<String>)> {
    let provider_configs = settings.get("providerConfigs").and_then(Value::as_object)?;
    let provider_key = settings
        .get("defaultModelKey")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|model_key| {
            settings
                .pointer("/modelCatalog/models")
                .and_then(Value::as_array)
                .and_then(|models| {
                    models.iter().find(|model| {
                        model
                            .get("key")
                            .or_else(|| model.get("model"))
                            .and_then(Value::as_str)
                            == Some(model_key)
                    })
                })
                .and_then(|model| model.get("provider"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        });
    if let Some(key) = provider_key {
        if let Some(config) = provider_configs.get(key) {
            return Some((config, Some(key.to_string())));
        }
    }
    provider_configs
        .values()
        .next()
        .map(|config| (config, None))
}

fn gateway_profile_tool_for_cli(cli_key: GatewayCliKey) -> Option<&'static str> {
    match cli_key {
        GatewayCliKey::Claude => Some("claude"),
        GatewayCliKey::ClaudeDesktop => Some("claude_desktop"),
        GatewayCliKey::Codex => Some("codex"),
        GatewayCliKey::Grok => Some("grok"),
        // Kimi has no verified built-in endpoint catalog yet
        // (`SUPPORTED_PROFILE_TOOLS` and `gateway_provider_profiles.json`
        // both exclude it), so a stored `gatewayProfile` reference must not
        // resolve. Add it here together with the catalog once endpoints are
        // verified.
        GatewayCliKey::Kimi => None,
        GatewayCliKey::Gemini => Some("gemini"),
        GatewayCliKey::OpenCode => None,
    }
}

#[derive(Debug, Clone, Default)]
struct GrokSelectedModel {
    model_id: Option<String>,
    base_url: Option<String>,
    api_backend: Option<String>,
    api_key: Option<String>,
}

fn grok_selected_model_from_settings(
    settings: &Value,
    config_toml: &str,
) -> Option<GrokSelectedModel> {
    if !config_toml.trim().is_empty() {
        if let Some(model) = grok_selected_model_from_toml(config_toml) {
            return Some(model);
        }
    }
    let default_key = settings
        .get("defaultModelKey")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let models = settings.pointer("/modelCatalog/models")?.as_array()?;
    let model = default_key
        .and_then(|key| {
            models.iter().find(|model| {
                model
                    .get("key")
                    .or_else(|| model.get("model"))
                    .and_then(Value::as_str)
                    == Some(key)
            })
        })
        .or_else(|| models.first())?;
    Some(GrokSelectedModel {
        model_id: json_value_string(model, "model").or_else(|| json_value_string(model, "key")),
        base_url: json_value_string(model, "baseUrl")
            .or_else(|| json_value_string(model, "base_url")),
        api_backend: json_value_string(model, "apiBackend")
            .or_else(|| json_value_string(model, "api_backend")),
        api_key: json_value_string(model, "apiKey").or_else(|| json_value_string(model, "api_key")),
    })
}

fn grok_selected_model_from_toml(config_toml: &str) -> Option<GrokSelectedModel> {
    let document = config_toml.parse::<toml_edit::DocumentMut>().ok()?;
    let root = document.as_table();
    let default_key = root
        .get("models")
        .and_then(toml_edit::Item::as_table)
        .and_then(|models| models.get("default"))
        .and_then(toml_edit::Item::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let models = root.get("model").and_then(toml_edit::Item::as_table)?;
    let table = default_key
        .and_then(|key| models.get(key))
        .and_then(toml_edit::Item::as_table)
        .or_else(|| models.iter().find_map(|(_, item)| item.as_table()))?;
    let string = |key: &str| {
        table
            .get(key)
            .and_then(toml_edit::Item::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    Some(GrokSelectedModel {
        model_id: string("model").or_else(|| default_key.map(str::to_string)),
        base_url: string("base_url"),
        api_backend: string("api_backend"),
        api_key: string("api_key"),
    })
}

fn gateway_profile_reference_matches_tool(
    reference: &GatewayProviderProfileReference,
    expected_tool: &str,
) -> bool {
    reference
        .tool
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_none_or(|tool| tool.eq_ignore_ascii_case(expected_tool))
}

fn find_gateway_profile_endpoint<'a>(
    catalog: &'a Value,
    tool: &str,
    profile_id: &str,
    endpoint_id: &str,
) -> Option<(&'a Value, &'a Value)> {
    let profile = catalog
        .get("profiles")
        .and_then(Value::as_array)?
        .iter()
        .find(|profile| {
            profile
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .is_some_and(|id| id == profile_id)
        })?;
    let endpoint = profile
        .get("tools")
        .and_then(|tools| tools.get(tool))
        .and_then(|tool_profile| tool_profile.get("endpoints"))
        .and_then(Value::as_array)?
        .iter()
        .find(|endpoint| {
            endpoint
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .is_some_and(|id| id == endpoint_id)
        })?;
    Some((profile, endpoint))
}

fn codex_chat_reasoning_from_profile_value(value: &Value) -> Option<CodexChatReasoningMeta> {
    value
        .get("codex_chat_reasoning")
        .or_else(|| value.get("codexChatReasoning"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
}

/// Resolve provider-level custom header overrides from persisted meta.
///
/// Reads `customHeaders` (or snake `custom_headers`) as an array of
/// `CustomHeaderOverride`. For pre-upgrade configs the legacy
/// `customUserAgent`/`custom_user_agent` string is folded into a single
/// `set` row on `User-Agent` — this is a read-only migration; the write
/// path only ever persists `customHeaders`, so a saved record stops
/// carrying the legacy key after its next edit.
fn custom_headers_from_meta(meta_value: &Value) -> Option<Vec<CustomHeaderOverride>> {
    let from_array = meta_value
        .get("custom_headers")
        .or_else(|| meta_value.get("customHeaders"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    serde_json::from_value::<CustomHeaderOverride>(item.clone()).ok()
                })
                .collect::<Vec<_>>()
        });
    if let Some(headers) = from_array {
        if !headers.is_empty() {
            return Some(headers);
        }
    }
    // Read-only legacy fallback: synthesize a `set` row from customUserAgent.
    json_string_compat(meta_value, "custom_user_agent", "customUserAgent").map(|user_agent| {
        vec![CustomHeaderOverride {
            op: "set".to_string(),
            name: "User-Agent".to_string(),
            value: user_agent,
            ..Default::default()
        }]
    })
}

fn model_rewrites_from_meta(meta_value: &Value) -> Option<Vec<ModelRewriteRule>> {
    let rewrites = meta_value
        .get("model_rewrites")
        .or_else(|| meta_value.get("modelRewrites"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value::<ModelRewriteRule>(item.clone()).ok())
                .filter(|rule| !rule.from.trim().is_empty() && !rule.to.trim().is_empty())
                .collect::<Vec<_>>()
        })?;
    if rewrites.is_empty() {
        None
    } else {
        Some(rewrites)
    }
}

fn model_rewrite_rules_from_meta(meta: &ProviderGatewayMeta) -> Vec<ModelRewriteRule> {
    meta.model_rewrites.clone().unwrap_or_default()
}

fn json_string_compat(value: &Value, snake_key: &str, camel_key: &str) -> Option<String> {
    value
        .get(snake_key)
        .or_else(|| value.get(camel_key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn json_bool_compat(value: &Value, snake_key: &str, camel_key: &str) -> Option<bool> {
    value
        .get(snake_key)
        .or_else(|| value.get(camel_key))
        .and_then(json_bool_value)
}

fn json_i64_compat(value: &Value, snake_key: &str, camel_key: &str) -> Option<i64> {
    value
        .get(snake_key)
        .or_else(|| value.get(camel_key))
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str()?.trim().parse::<i64>().ok())
        })
}

fn json_string_vec_compat(value: &Value, snake_key: &str, camel_key: &str) -> Vec<String> {
    value
        .get(snake_key)
        .or_else(|| value.get(camel_key))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn merge_model_catalog_image_capabilities(
    meta: &mut ProviderGatewayMeta,
    settings_config: Option<&Value>,
) {
    let Some(settings_config) = settings_config else {
        return;
    };
    let settings_value = match settings_config {
        Value::String(text) => serde_json::from_str::<Value>(text).ok(),
        Value::Object(_) => Some(settings_config.clone()),
        _ => None,
    };
    let Some(settings_value) = settings_value else {
        return;
    };
    let Some(models) = settings_value
        .get("modelCatalog")
        .and_then(|catalog| catalog.get("models"))
        .or_else(|| settings_value.get("models"))
        .and_then(Value::as_array)
    else {
        return;
    };

    for model in models {
        let Some(model_id) = model_catalog_model_id(model) else {
            continue;
        };
        match model_catalog_image_support(model) {
            Some(true) => push_unique_string(&mut meta.image_capable_models, model_id),
            Some(false) => push_unique_string(&mut meta.text_only_models, model_id),
            None => {}
        }
    }
}

fn model_catalog_model_id(model: &Value) -> Option<String> {
    [
        model.get("model"),
        model.get("id"),
        model.get("name"),
        model.get("modelId"),
        model.get("model_id"),
    ]
    .into_iter()
    .flatten()
    .find_map(Value::as_str)
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .map(str::to_string)
}

fn model_catalog_image_support(model: &Value) -> Option<bool> {
    if let Some(value) = model
        .get("supportsImage")
        .or_else(|| model.get("supports_image"))
        .or_else(|| model.get("vision"))
        .or_else(|| model.get("attachment"))
        .and_then(Value::as_bool)
    {
        return Some(value);
    }
    [
        model.get("input"),
        model.pointer("/modalities/input"),
        model.get("inputModalities"),
        model.get("input_modalities"),
    ]
    .into_iter()
    .flatten()
    .find_map(input_modalities_support_image)
}

fn input_modalities_support_image(value: &Value) -> Option<bool> {
    let modalities = value.as_array()?;
    Some(modalities.iter().any(|item| {
        item.as_str()
            .map(str::trim)
            .is_some_and(|item| item.eq_ignore_ascii_case("image"))
    }))
}

fn push_unique_string(items: &mut Vec<String>, item: String) {
    if !items.iter().any(|existing| existing == &item) {
        items.push(item);
    }
}

fn json_bool_value(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(value) => Some(*value),
        Value::Number(value) => value.as_i64().map(|value| value != 0),
        Value::String(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            if normalized.is_empty() {
                None
            } else {
                Some(matches!(normalized.as_str(), "true" | "1" | "yes" | "on"))
            }
        }
        _ => None,
    }
}

fn claude_target_protocol(meta: &ProviderGatewayMeta, settings: &Value) -> AiProtocol {
    meta.api_format
        .as_deref()
        .and_then(AiProtocol::from_api_format)
        .or_else(|| {
            json_value_string(settings, "api_format")
                .and_then(|value| AiProtocol::from_api_format(&value))
        })
        .or_else(|| {
            json_value_string(settings, "apiFormat")
                .and_then(|value| AiProtocol::from_api_format(&value))
        })
        .or_else(|| {
            settings
                .get("openrouter_compat_mode")
                .and_then(json_bool_value)
                .filter(|enabled| *enabled)
                .map(|_| AiProtocol::OpenAiChat)
        })
        .unwrap_or(AiProtocol::AnthropicMessages)
}

fn codex_target_protocol(
    meta: &ProviderGatewayMeta,
    settings: &Value,
    base_url: &str,
) -> AiProtocol {
    meta.api_format
        .as_deref()
        .and_then(AiProtocol::from_api_format)
        .or_else(|| {
            json_value_string(settings, "api_format")
                .and_then(|value| AiProtocol::from_api_format(&value))
        })
        .or_else(|| {
            json_value_string(settings, "apiFormat")
                .and_then(|value| AiProtocol::from_api_format(&value))
        })
        .or_else(|| {
            codex_wire_api_from_settings(settings)
                .and_then(|value| AiProtocol::from_api_format(&value))
        })
        .unwrap_or_else(|| {
            if is_chat_completions_url(base_url) {
                AiProtocol::OpenAiChat
            } else {
                AiProtocol::OpenAiResponses
            }
        })
}

fn gemini_target_protocol(meta: &ProviderGatewayMeta, settings: &Value) -> AiProtocol {
    meta.api_format
        .as_deref()
        .and_then(AiProtocol::from_api_format)
        .or_else(|| {
            json_value_string(settings, "api_format")
                .and_then(|value| AiProtocol::from_api_format(&value))
        })
        .or_else(|| {
            json_value_string(settings, "apiFormat")
                .and_then(|value| AiProtocol::from_api_format(&value))
        })
        .unwrap_or(AiProtocol::GeminiNative)
}

fn codex_wire_api_from_settings(settings: &Value) -> Option<String> {
    settings
        .get("config")
        .and_then(Value::as_str)
        .and_then(super::super::provider_protocol::codex_wire_api_from_config)
}

fn is_chat_completions_url(url: &str) -> bool {
    let normalized = url.trim_end_matches('/').to_ascii_lowercase();
    normalized.ends_with("/chat/completions") || normalized.contains("/chat/completions?")
}

fn claude_auth_from_settings(
    env: Option<&serde_json::Map<String, Value>>,
    settings: &Value,
    target_protocol: AiProtocol,
    api_key_field: Option<&str>,
    provider_name: &str,
) -> Result<(String, ProviderAuthStrategy), String> {
    let candidates: &[&str] = match target_protocol {
        AiProtocol::GeminiNative => &[
            "GEMINI_API_KEY",
            "GOOGLE_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_API_KEY",
        ],
        AiProtocol::OpenAiChat | AiProtocol::OpenAiResponses => &[
            "OPENAI_API_KEY",
            "OPENROUTER_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_API_KEY",
        ],
        AiProtocol::AnthropicMessages => &["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"],
    };

    for key_name in candidates {
        if let Some(api_key) = json_object_string(env, key_name) {
            let default_strategy = match (*key_name, target_protocol) {
                ("ANTHROPIC_AUTH_TOKEN", AiProtocol::AnthropicMessages) => {
                    ProviderAuthStrategy::Bearer
                }
                (_, AiProtocol::AnthropicMessages) => ProviderAuthStrategy::AnthropicApiKey,
                (_, AiProtocol::GeminiNative) => gemini_auth_strategy(&api_key),
                _ => ProviderAuthStrategy::Bearer,
            };
            return Ok((
                api_key.clone(),
                auth_strategy_for_target_protocol(
                    target_protocol,
                    api_key_field,
                    &api_key,
                    default_strategy,
                ),
            ));
        }
    }

    if let Some(api_key) =
        json_value_string(settings, "apiKey").or_else(|| json_value_string(settings, "api_key"))
    {
        let default_strategy = match target_protocol {
            AiProtocol::AnthropicMessages => ProviderAuthStrategy::AnthropicApiKey,
            AiProtocol::GeminiNative => gemini_auth_strategy(&api_key),
            AiProtocol::OpenAiChat | AiProtocol::OpenAiResponses => ProviderAuthStrategy::Bearer,
        };
        return Ok((
            api_key.clone(),
            auth_strategy_for_target_protocol(
                target_protocol,
                api_key_field,
                &api_key,
                default_strategy,
            ),
        ));
    }

    Err(format!(
        "Claude provider '{}' has no API key",
        provider_name
    ))
}

fn gemini_auth_from_settings(
    env: Option<&serde_json::Map<String, Value>>,
    settings: &Value,
    target_protocol: AiProtocol,
    api_key_field: Option<&str>,
    provider_name: &str,
) -> Result<(String, ProviderAuthStrategy), String> {
    let candidates: &[&str] = match target_protocol {
        AiProtocol::AnthropicMessages => &[
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "GEMINI_API_KEY",
            "GOOGLE_API_KEY",
        ],
        AiProtocol::OpenAiChat | AiProtocol::OpenAiResponses => {
            &["OPENAI_API_KEY", "GEMINI_API_KEY", "GOOGLE_API_KEY"]
        }
        AiProtocol::GeminiNative => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
    };

    for key_name in candidates {
        if let Some(api_key) = json_object_string(env, key_name) {
            let default_strategy = match (*key_name, target_protocol) {
                ("ANTHROPIC_AUTH_TOKEN", AiProtocol::AnthropicMessages) => {
                    ProviderAuthStrategy::Bearer
                }
                (_, AiProtocol::AnthropicMessages) => ProviderAuthStrategy::AnthropicApiKey,
                (_, AiProtocol::GeminiNative) => gemini_auth_strategy(&api_key),
                _ => ProviderAuthStrategy::Bearer,
            };
            return Ok((
                api_key.clone(),
                auth_strategy_for_target_protocol(
                    target_protocol,
                    api_key_field,
                    &api_key,
                    default_strategy,
                ),
            ));
        }
    }

    if let Some(api_key) =
        json_value_string(settings, "apiKey").or_else(|| json_value_string(settings, "api_key"))
    {
        let default_strategy = match target_protocol {
            AiProtocol::AnthropicMessages => ProviderAuthStrategy::AnthropicApiKey,
            AiProtocol::GeminiNative => gemini_auth_strategy(&api_key),
            AiProtocol::OpenAiChat | AiProtocol::OpenAiResponses => ProviderAuthStrategy::Bearer,
        };
        return Ok((
            api_key.clone(),
            auth_strategy_for_target_protocol(
                target_protocol,
                api_key_field,
                &api_key,
                default_strategy,
            ),
        ));
    }

    Err(format!(
        "Gemini CLI provider '{}' has no API key",
        provider_name
    ))
}

fn auth_strategy_for_target_protocol(
    target_protocol: AiProtocol,
    api_key_field: Option<&str>,
    api_key: &str,
    default_strategy: ProviderAuthStrategy,
) -> ProviderAuthStrategy {
    if let Some(strategy) = auth_strategy_from_api_key_field(api_key_field) {
        return strategy;
    }

    match target_protocol {
        AiProtocol::AnthropicMessages => default_strategy,
        AiProtocol::OpenAiChat | AiProtocol::OpenAiResponses => ProviderAuthStrategy::Bearer,
        AiProtocol::GeminiNative => gemini_auth_strategy(api_key),
    }
}

fn auth_strategy_from_api_key_field(value: Option<&str>) -> Option<ProviderAuthStrategy> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "authorization" | "bearer" | "auth_token" | "anthropic_auth_token" => {
            Some(ProviderAuthStrategy::Bearer)
        }
        "x-api-key" | "api_key" | "anthropic_api_key" => {
            Some(ProviderAuthStrategy::AnthropicApiKey)
        }
        "x-goog-api-key" | "google_api_key" | "gemini_api_key" => {
            Some(ProviderAuthStrategy::GoogleApiKey)
        }
        "google_oauth" | "oauth" => Some(ProviderAuthStrategy::GoogleOAuth),
        _ => None,
    }
}

fn gemini_auth_strategy(api_key: &str) -> ProviderAuthStrategy {
    let trimmed = api_key.trim();
    if trimmed.starts_with("ya29.") {
        return ProviderAuthStrategy::GoogleOAuth;
    }
    if trimmed.starts_with('{')
        && serde_json::from_str::<Value>(trimmed)
            .ok()
            .and_then(|value| {
                value
                    .get("access_token")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            })
            .is_some()
    {
        return ProviderAuthStrategy::GoogleOAuth;
    }
    ProviderAuthStrategy::GoogleApiKey
}

fn parse_json_config(raw: &str, label: &str) -> Result<Value, String> {
    serde_json::from_str(raw).map_err(|error| format!("Failed to parse {label}: {error}"))
}

fn claude_model_mapping_from_settings(settings: &Value) -> UpstreamModelMapping {
    let env = settings.get("env").and_then(Value::as_object);
    UpstreamModelMapping {
        default_model: json_value_string(settings, "model")
            .or_else(|| json_object_string(env, "ANTHROPIC_MODEL")),
        haiku_model: json_value_string(settings, "haikuModel")
            .or_else(|| json_object_string(env, "ANTHROPIC_DEFAULT_HAIKU_MODEL")),
        sonnet_model: json_value_string(settings, "sonnetModel")
            .or_else(|| json_object_string(env, "ANTHROPIC_DEFAULT_SONNET_MODEL")),
        opus_model: json_value_string(settings, "opusModel")
            .or_else(|| json_object_string(env, "ANTHROPIC_DEFAULT_OPUS_MODEL")),
        fable_model: json_value_string(settings, "fableModel")
            .or_else(|| json_object_string(env, "ANTHROPIC_DEFAULT_FABLE_MODEL")),
        reasoning_model: json_value_string(settings, "reasoningModel")
            .or_else(|| json_object_string(env, "ANTHROPIC_REASONING_MODEL")),
        ..UpstreamModelMapping::default()
    }
}

/// Claude Desktop model mapping. Starts from env/settings defaults (for the
/// `model` fallback + reasoning slot) then overlays the role routes stored in
/// `meta.claudeDesktopModelRoutes` — the single source of truth that also
/// drives the 3P profile's `inferenceModels`. The 3P app only ever sends the
/// claude-safe route IDs (`claude-sonnet-5` …), so the gateway rewrites them to
/// the real upstream models via this mapping.
fn claude_desktop_model_mapping(meta: &Option<Value>, settings: &Value) -> UpstreamModelMapping {
    let mut mapping = claude_model_mapping_from_settings(settings);
    let Some(routes) = meta
        .as_ref()
        .and_then(|m| {
            m.get("claudeDesktopModelRoutes")
                .or_else(|| m.get("claude_desktop_model_routes"))
        })
        .and_then(Value::as_object)
    else {
        return mapping;
    };
    for (route_id, route) in routes {
        let Some(model) = route
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|model| !model.is_empty())
        else {
            continue;
        };
        let id = route_id.to_ascii_lowercase();
        if id.contains("sonnet") {
            mapping.sonnet_model = Some(model.to_string());
        } else if id.contains("opus") {
            mapping.opus_model = Some(model.to_string());
        } else if id.contains("fable") {
            mapping.fable_model = Some(model.to_string());
        } else if id.contains("haiku") {
            mapping.haiku_model = Some(model.to_string());
        }
    }
    mapping
}

fn json_value_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(super) fn json_object_string(
    object: Option<&serde_json::Map<String, Value>>,
    key: &str,
) -> Option<String> {
    object
        .and_then(|object| object.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(super) fn codex_base_url_from_config(config_toml: &str) -> Option<String> {
    super::super::provider_protocol::codex_base_url_from_config(config_toml)
}

fn codex_model_from_config(config_toml: &str) -> Option<String> {
    super::super::provider_protocol::codex_model_from_config(config_toml)
}

fn codex_auto_review_model_from_settings(settings: &Value) -> Option<String> {
    settings
        .get("autoReviewModelOverride")
        .or_else(|| settings.get("auto_review_model_override"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

// Match the Codex module's legacy modelCatalog row-level override.
// This covers short-lived drafts and older provider records.
fn codex_auto_review_model_from_catalog(catalog: &Value) -> Option<String> {
    let models = catalog.get("models").and_then(|m| m.as_array())?;
    for item in models {
        if let Some(legacy) = item
            .get("autoReviewModelOverride")
            .or_else(|| item.get("auto_review_model_override"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
        {
            return Some(legacy);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a single/failover selection with the aggregate fields defaulted.
    fn selection_for(
        mode: GatewayProxyMode,
        primary_provider_id: &str,
    ) -> GatewayProviderSelection {
        GatewayProviderSelection {
            mode,
            primary_provider_id: primary_provider_id.to_string(),
            aggregate_provider_ids: Vec::new(),
            aggregate_separator:
                crate::coding::proxy_gateway::cli_proxy::manifest::AGGREGATE_DEFAULT_SEPARATOR
                    .to_string(),
            aggregate_aliases: std::collections::BTreeMap::new(),
            aggregate_naming: AggregateNamingMode::default(),
            aggregate_groups: Vec::new(),
        }
    }

    fn provider(name: &str, sort_index: Option<i32>) -> UpstreamProvider {
        UpstreamProvider {
            supports_websockets: None,
            cli_key: GatewayCliKey::Claude,
            id: name.to_string(),
            name: name.to_string(),
            base_url: "https://api.example.com".to_string(),
            api_key: "key".to_string(),
            target_protocol: AiProtocol::AnthropicMessages,
            auth_strategy: ProviderAuthStrategy::AnthropicApiKey,
            is_full_url: false,
            sort_index,
            meta: ProviderGatewayMeta::default(),
            model_mapping: UpstreamModelMapping::default(),
        }
    }

    fn aggregate_selection(ids: &[&str], separator: &str) -> GatewayProviderSelection {
        GatewayProviderSelection {
            mode: GatewayProxyMode::Aggregate,
            primary_provider_id: ids.first().copied().unwrap_or_default().to_string(),
            aggregate_provider_ids: ids.iter().map(|id| id.to_string()).collect(),
            aggregate_separator: separator.to_string(),
            aggregate_aliases: std::collections::BTreeMap::new(),
            aggregate_naming: AggregateNamingMode::default(),
            aggregate_groups: Vec::new(),
        }
    }

    #[test]
    fn aggregate_selection_keeps_every_candidate() {
        let providers = vec![
            provider("site-a", Some(10)),
            provider("site-b", Some(20)),
            provider("site-c", Some(30)),
        ];

        let selection = aggregate_selection(&["site-c", "site-a"], ".");
        let selected = apply_provider_selection(providers, Some(&selection)).unwrap();

        // Selected sites are promoted in the user's order; the rest stay as
        // fallbacks instead of being dropped.
        let ids: Vec<&str> = selected.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["site-c", "site-a", "site-b"]);
    }

    #[test]
    fn aggregate_selection_without_sites_keeps_candidates_in_order() {
        let providers = vec![provider("site-a", Some(10)), provider("site-b", Some(20))];

        let selection = aggregate_selection(&[], ".");
        let selected = apply_provider_selection(providers, Some(&selection)).unwrap();

        let ids: Vec<&str> = selected.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["site-a", "site-b"]);
    }

    #[test]
    fn aggregate_selection_ignores_unknown_site_ids() {
        let providers = vec![provider("site-a", Some(10))];

        let selection = aggregate_selection(&["missing", "site-a"], ".");
        let selected = apply_provider_selection(providers, Some(&selection)).unwrap();

        let ids: Vec<&str> = selected.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["site-a"]);
    }

    #[test]
    fn strict_aggregate_selection_uses_group_union_not_legacy_provider_ids() {
        let providers = vec![
            provider("group-a", Some(10)),
            provider("group-b", Some(20)),
            provider("ungrouped", Some(30)),
        ];
        let selection = GatewayProviderSelection {
            mode: GatewayProxyMode::Aggregate,
            primary_provider_id: "ungrouped".to_string(),
            // This stale legacy list must not control strict routing.
            aggregate_provider_ids: vec!["ungrouped".to_string()],
            aggregate_separator: ">".to_string(),
            aggregate_aliases: std::collections::BTreeMap::new(),
            aggregate_naming: AggregateNamingMode::ModelOnly,
            aggregate_groups: vec![GatewayAggregateGroup {
                id: "fast".to_string(),
                provider_ids: vec!["group-b".to_string(), "group-a".to_string()],
            }],
        };

        let selected = apply_provider_selection(providers, Some(&selection)).unwrap();
        let ids: Vec<&str> = selected.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["group-b", "group-a"]);
    }

    #[test]
    fn strict_aggregate_selection_rejects_stale_group_provider() {
        let selection = GatewayProviderSelection {
            mode: GatewayProxyMode::Aggregate,
            primary_provider_id: "group-a".to_string(),
            aggregate_provider_ids: vec!["group-a".to_string()],
            aggregate_separator: ".".to_string(),
            aggregate_aliases: std::collections::BTreeMap::new(),
            aggregate_naming: AggregateNamingMode::default(),
            aggregate_groups: vec![GatewayAggregateGroup {
                id: "fast".to_string(),
                provider_ids: vec!["missing".to_string()],
            }],
        };

        let error = apply_provider_selection(vec![provider("group-a", Some(10))], Some(&selection))
            .expect_err("stale strict groups must fail closed");
        assert!(error.contains("not available for Gateway proxy"));
    }

    #[test]
    fn aggregate_route_splits_site_prefix() {
        let providers = vec![provider("site1", None), provider("site2", None)];

        let route = resolve_aggregate_route("site1/deepseek-v4-flash", "/", &providers);

        assert_eq!(route.site_id.as_deref(), Some("site1"));
        assert_eq!(route.upstream_model, "deepseek-v4-flash");
    }

    #[test]
    fn aggregate_route_uses_default_dot_separator() {
        let providers = vec![provider("76a6ef74", None)];

        let route = resolve_aggregate_route(
            "76a6ef74.deepseek-v4.1-flash-expires-on-0910",
            ".",
            &providers,
        );

        assert_eq!(route.site_id.as_deref(), Some("76a6ef74"));
        // Only the site prefix is stripped; dots inside the model name survive.
        assert_eq!(route.upstream_model, "deepseek-v4.1-flash-expires-on-0910");
    }

    #[test]
    fn aggregate_route_uses_resolved_display_name_and_keeps_legacy_provider_slug() {
        let provider_id = "76a6ef74af6c4151812787cc519b534b";
        let providers = vec![provider(provider_id, None)];
        let mut selection = aggregate_selection(&[provider_id], ".");
        selection
            .aggregate_aliases
            .insert(provider_id.to_string(), "思辰888".to_string());

        let display_name_route = resolve_aggregate_route_with_selection(
            "思辰888.deepseek-v4.1-flash",
            &selection,
            &providers,
        )
        .unwrap();
        assert_eq!(display_name_route.site_id.as_deref(), Some(provider_id));
        assert_eq!(display_name_route.upstream_model, "deepseek-v4.1-flash");

        // Existing Codex selections that used the raw provider id keep routing
        // after the catalog moves to the human-readable default prefix.
        let legacy_route = resolve_aggregate_route_with_selection(
            "76a6ef74af6c4151812787cc519b534b.deepseek-v4.1-flash",
            &selection,
            &providers,
        )
        .unwrap();
        assert_eq!(legacy_route.site_id.as_deref(), Some(provider_id));
        assert_eq!(legacy_route.upstream_model, "deepseek-v4.1-flash");

        selection.aggregate_naming = AggregateNamingMode::ModelAtSite;
        selection.aggregate_separator = "@".to_string();
        let legacy_model_at_site_route = resolve_aggregate_route_with_selection(
            "deepseek-v4.1-flash@76a6ef74af6c4151812787cc519b534b",
            &selection,
            &providers,
        )
        .unwrap();
        assert_eq!(
            legacy_model_at_site_route.site_id.as_deref(),
            Some(provider_id)
        );
        assert_eq!(
            legacy_model_at_site_route.upstream_model,
            "deepseek-v4.1-flash"
        );
    }

    #[test]
    fn aggregate_route_handles_dashed_and_underscored_site_ids() {
        let providers = vec![provider("nofx-a656", None), provider("site_2", None)];

        let dash = resolve_aggregate_route("nofx-a656.gpt-5.6-sol", ".", &providers);
        assert_eq!(dash.site_id.as_deref(), Some("nofx-a656"));
        assert_eq!(dash.upstream_model, "gpt-5.6-sol");

        let underscore = resolve_aggregate_route("site_2.glm-5.3-flash", ".", &providers);
        assert_eq!(underscore.site_id.as_deref(), Some("site_2"));
        assert_eq!(underscore.upstream_model, "glm-5.3-flash");
    }

    #[test]
    fn aggregate_route_falls_back_to_bare_model_without_prefix() {
        let providers = vec![provider("site1", None)];

        let route = resolve_aggregate_route("deepseek-v4-flash", ".", &providers);

        assert_eq!(route.site_id, None);
        assert_eq!(route.upstream_model, "deepseek-v4-flash");
    }

    #[test]
    fn strict_aggregate_route_requires_group_slug_and_rejects_legacy_fallbacks() {
        let providers = vec![provider("provider-a", None), provider("provider-b", None)];
        let selection = GatewayProviderSelection {
            mode: GatewayProxyMode::Aggregate,
            primary_provider_id: "provider-a".to_string(),
            aggregate_provider_ids: vec!["provider-a".to_string(), "provider-b".to_string()],
            aggregate_separator: ">".to_string(),
            aggregate_aliases: std::collections::BTreeMap::new(),
            aggregate_naming: AggregateNamingMode::ModelOnly,
            aggregate_groups: vec![
                GatewayAggregateGroup {
                    id: "fast".to_string(),
                    provider_ids: vec!["provider-a".to_string()],
                },
                GatewayAggregateGroup {
                    id: "slow".to_string(),
                    provider_ids: vec!["provider-b".to_string()],
                },
            ],
        };

        let route = resolve_aggregate_route_with_selection("fast.gpt-5", &selection, &providers)
            .expect("canonical strict group slug");
        assert_eq!(route.site_id, None);
        assert_eq!(route.group_id.as_deref(), Some("fast"));
        assert_eq!(route.upstream_model, "gpt-5");
        assert!(route.explicit);

        for invalid_slug in [
            "gpt-5",            // bare model: no group selection
            "provider-a.gpt-5", // legacy site prefix
            "gpt-5>provider-a", // legacy model-at-site separator/template
            "missing.gpt-5",    // unknown group
        ] {
            assert!(
                resolve_aggregate_route_with_selection(invalid_slug, &selection, &providers)
                    .is_err(),
                "strict groups must reject non-canonical slug {invalid_slug:?}"
            );
        }
    }

    #[test]
    fn declared_models_include_catalog_root_and_default_model_sources() {
        let settings = serde_json::json!({
            "modelCatalog": {
                "models": [
                    { "model": "catalog-model" },
                    { "id": "shared-model" }
                ]
            },
            "models": [
                { "name": "root-model" },
                { "model": "shared-model" }
            ],
            "config": "model = \"default-model\"\n"
        });

        assert_eq!(
            declared_models_from_settings(Some(&settings), true),
            vec![
                "catalog-model".to_string(),
                "shared-model".to_string(),
                "root-model".to_string(),
                "default-model".to_string(),
            ]
        );
    }

    #[test]
    fn strict_declared_models_accept_string_rows_and_chat_model_precedence() {
        let settings = serde_json::json!({
            "modelCatalog": {
                "models": ["catalog-shorthand", { "model": "catalog-object" }]
            },
            "models": ["root-shorthand"],
            "config": "model = \"root-default\"\n[chat]\nmodel = \"chat-default\"\n"
        });

        assert_eq!(
            declared_models_from_settings(Some(&settings), true),
            vec![
                "catalog-shorthand".to_string(),
                "catalog-object".to_string(),
                "root-shorthand".to_string(),
                "chat-default".to_string(),
            ]
        );
    }

    #[test]
    fn legacy_declared_models_ignore_string_rows() {
        let settings = serde_json::json!({
            "modelCatalog": { "models": ["catalog-shorthand", { "model": "catalog-object" }] },
            "models": ["root-shorthand"]
        });

        assert_eq!(
            declared_models_from_settings(Some(&settings), false),
            vec!["catalog-object".to_string()]
        );
    }

    #[test]
    fn legacy_declared_models_keep_catalog_precedence() {
        let settings = serde_json::json!({
            "modelCatalog": { "models": [{ "model": "catalog-model" }] },
            "models": [{ "model": "root-model" }],
            "config": "model = \"default-model\"\n"
        });

        assert_eq!(
            declared_models_from_settings(Some(&settings), false),
            vec!["catalog-model".to_string()]
        );
    }

    #[test]
    fn aggregate_route_ignores_unknown_site_prefix() {
        let providers = vec![provider("site1", None)];

        // `site9` is not a candidate, so the whole string stays as the model
        // name and the caller resolves it by declared catalog membership.
        let route = resolve_aggregate_route("site9.deepseek-v4-flash", ".", &providers);

        assert_eq!(route.site_id, None);
        assert_eq!(route.upstream_model, "site9.deepseek-v4-flash");
    }

    #[test]
    fn aggregate_route_requires_separator_after_site_id() {
        let providers = vec![provider("site1", None)];

        // No separator between site id and model: not a prefixed request.
        let route = resolve_aggregate_route("site1deepseek", ".", &providers);

        assert_eq!(route.site_id, None);
    }

    #[test]
    fn aggregate_route_rejects_empty_model_after_separator() {
        let providers = vec![provider("site1", None)];

        let route = resolve_aggregate_route("site1.", ".", &providers);

        assert_eq!(route.site_id, None);
    }

    #[test]
    fn aggregate_route_supports_custom_separator() {
        let providers = vec![provider("site1", None)];

        let route = resolve_aggregate_route("site1>deepseek-v4-flash", ">", &providers);

        assert_eq!(route.site_id.as_deref(), Some("site1"));
        assert_eq!(route.upstream_model, "deepseek-v4-flash");
    }

    #[test]
    fn aggregate_separator_validation_rejects_ambiguous_values() {
        assert!(
            super::super::super::cli_proxy::manifest::validate_aggregate_separator(".").is_ok()
        );
        assert!(
            super::super::super::cli_proxy::manifest::validate_aggregate_separator("/").is_ok()
        );
        assert!(
            super::super::super::cli_proxy::manifest::validate_aggregate_separator("").is_err()
        );
        assert!(
            super::super::super::cli_proxy::manifest::validate_aggregate_separator("-").is_err()
        );
        assert!(
            super::super::super::cli_proxy::manifest::validate_aggregate_separator("_").is_err()
        );
        assert!(
            super::super::super::cli_proxy::manifest::validate_aggregate_separator("a").is_err()
        );
        assert!(
            super::super::super::cli_proxy::manifest::validate_aggregate_separator("思").is_err()
        );
        assert!(
            super::super::super::cli_proxy::manifest::validate_aggregate_separator(" ").is_err()
        );
    }

    #[test]
    fn candidate_providers_keep_topmost_sort_index_first() {
        let mut providers = vec![
            provider("third", Some(30)),
            provider("first", Some(10)),
            provider("second", Some(20)),
        ];

        sort_candidate_providers(&mut providers);

        let names: Vec<&str> = providers
            .iter()
            .map(|provider| provider.name.as_str())
            .collect();
        assert_eq!(names, vec!["first", "second", "third"]);
    }

    #[test]
    fn grok_selected_model_uses_default_toml_table() {
        let config = r#"
[models]
default = "custom"

[model.other]
model = "other-upstream"
base_url = "https://other.example.com/v1"
api_backend = "chat_completions"

[model.custom]
model = "custom-upstream"
base_url = "https://custom.example.com/v1"
api_backend = "messages"
api_key = "secret"
"#;
        let selected = grok_selected_model_from_toml(config).expect("selected model");
        assert_eq!(selected.model_id.as_deref(), Some("custom-upstream"));
        assert_eq!(
            selected.base_url.as_deref(),
            Some("https://custom.example.com/v1")
        );
        assert_eq!(selected.api_backend.as_deref(), Some("messages"));
        assert_eq!(selected.api_key.as_deref(), Some("secret"));
    }

    #[test]
    fn none_sort_index_treated_as_zero_matching_frontend() {
        let mut providers = vec![provider("second", Some(1)), provider("first", None)];

        sort_candidate_providers(&mut providers);

        let names: Vec<&str> = providers
            .iter()
            .map(|provider| provider.name.as_str())
            .collect();
        assert_eq!(names, vec!["first", "second"]);
    }

    #[test]
    fn single_selection_returns_only_primary_provider() {
        let providers = vec![
            provider("first", Some(10)),
            provider("primary", Some(20)),
            provider("third", Some(30)),
        ];
        let selection = selection_for(GatewayProxyMode::Single, "primary");

        let selected = apply_provider_selection(providers, Some(&selection)).unwrap();

        let names: Vec<&str> = selected
            .iter()
            .map(|provider| provider.name.as_str())
            .collect();
        assert_eq!(names, vec!["primary"]);
    }

    #[test]
    fn failover_selection_promotes_primary_provider_to_p0() {
        let providers = vec![
            provider("first", Some(10)),
            provider("primary", Some(20)),
            provider("third", Some(30)),
        ];
        let selection = selection_for(GatewayProxyMode::Failover, "primary");

        let selected = apply_provider_selection(providers, Some(&selection)).unwrap();

        let names: Vec<&str> = selected
            .iter()
            .map(|provider| provider.name.as_str())
            .collect();
        assert_eq!(names, vec!["primary", "first", "third"]);
    }

    #[test]
    fn failover_selection_keeps_sorted_order_when_primary_is_missing() {
        let providers = vec![provider("first", Some(10)), provider("second", Some(20))];
        let selection = selection_for(GatewayProxyMode::Failover, "missing");

        let selected = apply_provider_selection(providers, Some(&selection)).unwrap();

        let names: Vec<&str> = selected
            .iter()
            .map(|provider| provider.name.as_str())
            .collect();
        assert_eq!(names, vec!["first", "second"]);
    }

    #[test]
    fn kimi_provider_without_api_key_is_skipped_not_error() {
        // Adopted official-OAuth setups (credentials live outside config.toml)
        // carry no resolvable API key; they must not fail the whole candidate
        // list and turn the CLI takeover status into an error.
        let record = serde_json::json!({
            "id": "p1",
            "name": "Adopted Local",
            "category": "custom",
            "settings_config": serde_json::json!({
                "auth": {},
                "defaultModelKey": "kimi-code/k3",
                "providerConfigs": { "kimi-official": { "type": "kimi" } }
            })
            .to_string(),
            "sort_index": 0,
            "is_disabled": false,
        });
        let result =
            provider_from_record(GatewayCliKey::Kimi, record, None).expect("skip must not error");
        assert!(result.is_none());
    }

    #[test]
    fn kimi_provider_falls_back_to_provider_config_api_key() {
        let record = serde_json::json!({
            "id": "p2",
            "name": "Relay",
            "category": "custom",
            "settings_config": serde_json::json!({
                "auth": {},
                "defaultModelKey": "relay/k3",
                "providerConfigs": {
                    "relay": {
                        "type": "openai",
                        "base_url": "https://relay.example.com/v1",
                        "api_key": "sk-relay"
                    }
                }
            })
            .to_string(),
            "sort_index": 0,
        });
        let result = provider_from_record(GatewayCliKey::Kimi, record, None)
            .expect("provider ok")
            .expect("provider present");
        assert_eq!(result.api_key, "sk-relay");
        assert_eq!(result.base_url, "https://relay.example.com/v1");
    }

    #[test]
    fn kimi_custom_relay_record_loads_as_proxyable_candidate() {
        // Regression shape reported from the field: an openai-style relay
        // provider with auth.API_KEY and a catalog model without
        // maxContextSize. It must load as a proxyable candidate so the CLI
        // takeover status stays Direct (can_takeover) and the provider card
        // keeps its gateway proxy button.
        let record = serde_json::json!({
            "id": "relay-1",
            "name": "relay",
            "category": "custom",
            "settings_config": serde_json::json!({
                "providerConfigs": {
                    "custom": {
                        "type": "openai",
                        "base_url": "https://relay.example.com/v1"
                    }
                },
                "modelCatalog": {
                    "models": [
                        { "key": "custom-model", "model": "custom-model", "provider": "custom", "displayName": "Model" }
                    ]
                },
                "defaultModelKey": "custom-model",
                "auth": { "API_KEY": "sk-test-key" }
            })
            .to_string(),
            "sort_index": 1,
            "is_applied": true,
            "is_disabled": false,
        });
        let result = provider_from_record(GatewayCliKey::Kimi, record, None)
            .expect("candidate must load")
            .expect("candidate present");
        assert_eq!(result.api_key, "sk-test-key");
        assert_eq!(result.base_url, "https://relay.example.com/v1");
        assert_eq!(result.target_protocol, AiProtocol::OpenAiChat);
    }

    #[test]
    fn kimi_candidate_resolves_provider_through_model_catalog_chain() {
        // Multi-provider record: the first providerConfigs entry is NOT the
        // active upstream. The candidate must mirror the takeover patch chain
        // (defaultModelKey -> modelCatalog.models[].provider ->
        // providerConfigs[provider]) instead of picking the first entry.
        let record = serde_json::json!({
            "id": "multi",
            "name": "Multi",
            "category": "custom",
            "settings_config": serde_json::json!({
                "providerConfigs": {
                    "first": {
                        "type": "openai",
                        "base_url": "https://first.example.com/v1",
                        "api_key": "sk-first"
                    },
                    "axonhub": {
                        "type": "openai",
                        "base_url": "https://axonhub.example.com/v1",
                        "api_key": "sk-axonhub"
                    }
                },
                "modelCatalog": {
                    "models": [
                        { "key": "k2", "model": "k2", "provider": "axonhub" }
                    ]
                },
                "defaultModelKey": "k2"
            })
            .to_string(),
            "sort_index": 0,
            "is_disabled": false,
        });
        let result = provider_from_record(GatewayCliKey::Kimi, record, None)
            .expect("provider ok")
            .expect("provider present");
        assert_eq!(result.base_url, "https://axonhub.example.com/v1");
        assert_eq!(result.api_key, "sk-axonhub");
        assert_eq!(result.model_mapping.default_model.as_deref(), Some("k2"));
    }

    #[test]
    fn kimi_candidate_falls_back_to_first_provider_config_without_chain() {
        // Incomplete chain (no defaultModelKey / no catalog): the first
        // providerConfigs entry remains the fallback instead of skipping the
        // record.
        let record = serde_json::json!({
            "id": "chainless",
            "name": "Chainless",
            "category": "custom",
            "settings_config": serde_json::json!({
                "providerConfigs": {
                    "solo": {
                        "type": "openai",
                        "base_url": "https://solo.example.com/v1",
                        "api_key": "sk-solo"
                    }
                }
            })
            .to_string(),
            "sort_index": 0,
            "is_disabled": false,
        });
        let result = provider_from_record(GatewayCliKey::Kimi, record, None)
            .expect("provider ok")
            .expect("provider present");
        assert_eq!(result.base_url, "https://solo.example.com/v1");
        assert_eq!(result.api_key, "sk-solo");
        assert_eq!(result.model_mapping.default_model, None);
    }

    #[test]
    fn claude_model_mapping_reads_reasoning_model() {
        let mapping = claude_model_mapping_from_settings(&serde_json::json!({
            "model": "provider-default",
            "fableModel": "provider-fable",
            "reasoningModel": "provider-reasoning",
            "env": {
                "ANTHROPIC_REASONING_MODEL": "env-reasoning"
            }
        }));

        assert_eq!(mapping.default_model.as_deref(), Some("provider-default"));
        assert_eq!(mapping.fable_model.as_deref(), Some("provider-fable"));
        assert_eq!(
            mapping.reasoning_model.as_deref(),
            Some("provider-reasoning")
        );
    }

    #[test]
    fn claude_desktop_model_mapping_reads_role_routes_from_meta() {
        let mapping = claude_desktop_model_mapping(
            &Some(serde_json::json!({
                "claudeDesktopModelRoutes": {
                    "claude-sonnet-5": {"model": "deepseek-sonnet", "supports1m": false},
                    "claude-opus-5": {"model": "deepseek-v4-flash-0731[1M]"},
                    "claude-fable-5": {"model": "deepseek-fable"},
                    "claude-haiku-4-5": {"model": "deepseek-haiku"}
                }
            })),
            &serde_json::json!({}),
        );

        assert_eq!(mapping.sonnet_model.as_deref(), Some("deepseek-sonnet"));
        assert_eq!(
            mapping.opus_model.as_deref(),
            Some("deepseek-v4-flash-0731[1M]")
        );
        assert_eq!(mapping.fable_model.as_deref(), Some("deepseek-fable"));
        assert_eq!(mapping.haiku_model.as_deref(), Some("deepseek-haiku"));
    }

    #[test]
    fn claude_desktop_model_mapping_falls_back_to_settings_when_routes_missing() {
        let mapping = claude_desktop_model_mapping(
            &Some(serde_json::json!({})),
            &serde_json::json!({
                "sonnetModel": "settings-sonnet",
                "env": {"ANTHROPIC_DEFAULT_OPUS_MODEL": "settings-opus"}
            }),
        );

        assert_eq!(mapping.sonnet_model.as_deref(), Some("settings-sonnet"));
        assert_eq!(mapping.opus_model.as_deref(), Some("settings-opus"));
    }

    #[test]
    fn claude_desktop_model_mapping_ignores_empty_route_model() {
        let mapping = claude_desktop_model_mapping(
            &Some(serde_json::json!({
                "claudeDesktopModelRoutes": {
                    "claude-sonnet-5": {"model": "  ", "supports1m": false},
                    "claude-opus-5": {"model": "deepseek-opus"}
                }
            })),
            &serde_json::json!({}),
        );

        assert_eq!(mapping.sonnet_model, None);
        assert_eq!(mapping.opus_model.as_deref(), Some("deepseek-opus"));
    }

    #[test]
    fn codex_provider_loads_default_model_from_config_toml() {
        let result = provider_from_record(
            GatewayCliKey::Codex,
            serde_json::json!({
                "id": "codex-default-model",
                "name": "Codex Default Model",
                "category": "custom",
                "settings_config": serde_json::json!({
                    "auth": {"OPENAI_API_KEY": "test-key"},
                    "autoReviewModelOverride": "review-model",
                    "config": r#"
model = "deepseek-v4-flash"
model_provider = "custom"
[model_providers.custom]
base_url = "https://api.deepseek.com/v1"
"#
                }).to_string(),
                "is_disabled": false
            }),
            None,
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            result.model_mapping.default_model.as_deref(),
            Some("deepseek-v4-flash")
        );
        assert_eq!(
            result.model_mapping.auto_review_model.as_deref(),
            Some("review-model")
        );
    }

    #[test]
    fn codex_provider_loads_auto_review_model_from_model_catalog_legacy() {
        let result = provider_from_record(
            GatewayCliKey::Codex,
            serde_json::json!({
                "id": "codex-catalog-review",
                "name": "Codex Catalog Review",
                "category": "custom",
                "settings_config": serde_json::json!({
                    "auth": {"OPENAI_API_KEY": "test-key"},
                    "modelCatalog": {
                        "models": [
                            {
                                "model": "deepseek-v4-flash",
                                "autoReviewModelOverride": "catalog-review-model"
                            }
                        ]
                    },
                    "config": r#"
model = "deepseek-v4-flash"
model_provider = "custom"
[model_providers.custom]
base_url = "https://api.deepseek.com/v1"
"#
                }).to_string(),
                "is_disabled": false
            }),
            None,
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            result.model_mapping.auto_review_model.as_deref(),
            Some("catalog-review-model")
        );
    }

    #[test]
    fn codex_provider_top_level_auto_review_overrides_model_catalog_legacy() {
        let result = provider_from_record(
            GatewayCliKey::Codex,
            serde_json::json!({
                "id": "codex-top-level-review",
                "name": "Codex Top Level Review",
                "category": "custom",
                "settings_config": serde_json::json!({
                    "auth": {"OPENAI_API_KEY": "test-key"},
                    "autoReviewModelOverride": "top-level-review",
                    "modelCatalog": {
                        "models": [
                            {
                                "model": "deepseek-v4-flash",
                                "autoReviewModelOverride": "catalog-review-model"
                            }
                        ]
                    },
                    "config": r#"
model = "deepseek-v4-flash"
model_provider = "custom"
[model_providers.custom]
base_url = "https://api.deepseek.com/v1"
"#
                }).to_string(),
                "is_disabled": false
            }),
            None,
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            result.model_mapping.auto_review_model.as_deref(),
            Some("top-level-review")
        );
    }

    #[test]
    fn codex_provider_blank_auto_review_override_falls_back_to_catalog_then_none() {
        let with_catalog = provider_from_record(
            GatewayCliKey::Codex,
            serde_json::json!({
                "id": "codex-blank-top-level",
                "name": "Codex Blank Top Level",
                "category": "custom",
                "settings_config": serde_json::json!({
                    "auth": {"OPENAI_API_KEY": "test-key"},
                    "autoReviewModelOverride": "  ",
                    "modelCatalog": {
                        "models": [
                            {
                                "model": "deepseek-v4-flash",
                                "auto_review_model_override": "catalog-from-blank-top"
                            }
                        ]
                    },
                    "config": r#"
model = "deepseek-v4-flash"
model_provider = "custom"
[model_providers.custom]
base_url = "https://api.deepseek.com/v1"
"#
                }).to_string(),
                "is_disabled": false
            }),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            with_catalog.model_mapping.auto_review_model.as_deref(),
            Some("catalog-from-blank-top")
        );

        let blank_only = provider_from_record(
            GatewayCliKey::Codex,
            serde_json::json!({
                "id": "codex-blank-only",
                "name": "Codex Blank Only",
                "category": "custom",
                "settings_config": serde_json::json!({
                    "auth": {"OPENAI_API_KEY": "test-key"},
                    "autoReviewModelOverride": "  ",
                    "config": r#"
model = "deepseek-v4-flash"
model_provider = "custom"
[model_providers.custom]
base_url = "https://api.deepseek.com/v1"
"#
                }).to_string(),
                "is_disabled": false
            }),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(blank_only.model_mapping.auto_review_model, None);
    }

    #[test]
    fn codex_provider_loads_default_model_from_chat_config_toml() {
        let result = provider_from_record(
            GatewayCliKey::Codex,
            serde_json::json!({
                "id": "codex-chat-model",
                "name": "Codex Chat Model",
                "category": "custom",
                "settings_config": serde_json::json!({
                    "auth": {"OPENAI_API_KEY": "test-key"},
                    "config": r#"
model = "stale-root-model"
model_provider = "custom"
[chat]
model = "qwen3-coder"
[model_providers.custom]
base_url = "https://api.deepseek.com/v1"
"#
                }).to_string(),
                "is_disabled": false
            }),
            None,
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            result.model_mapping.default_model.as_deref(),
            Some("qwen3-coder")
        );
    }

    #[test]
    fn codex_provider_without_root_model_keeps_empty_default_model() {
        let result = provider_from_record(
            GatewayCliKey::Codex,
            serde_json::json!({
                "id": "codex-no-model",
                "name": "Codex No Model",
                "category": "custom",
                "settings_config": serde_json::json!({
                    "auth": {"OPENAI_API_KEY": "test-key"},
                    "config": r#"
model_provider = "custom"
[model_providers.custom]
base_url = "https://api.openai.com/v1"
"#
                }).to_string(),
                "is_disabled": false
            }),
            None,
        )
        .unwrap()
        .unwrap();

        assert_eq!(result.model_mapping.default_model, None);
    }

    #[test]
    fn official_providers_are_not_gateway_candidates() {
        for cli_key in [
            GatewayCliKey::Claude,
            GatewayCliKey::Codex,
            GatewayCliKey::Gemini,
        ] {
            let result = provider_from_record(
                cli_key,
                serde_json::json!({
                    "id": format!("{}-official", cli_key.as_str()),
                    "name": "Official",
                    "category": "official",
                    "settings_config": "{}",
                    "is_disabled": false
                }),
                None,
            )
            .unwrap();

            assert!(result.is_none());
        }
    }

    #[test]
    fn claude_raw_url_suffix_selects_openai_chat_target_and_bearer_auth() {
        let result = provider_from_record(
            GatewayCliKey::Claude,
            serde_json::json!({
                "id": "claude-openai-chat",
                "name": "Claude OpenAI Chat",
                "category": "custom",
                "settings_config": serde_json::json!({
                    "env": {
                        "ANTHROPIC_BASE_URL": "https://openrouter.example.com/v1/chat/completions##",
                        "OPENAI_API_KEY": "openai-key"
                    }
                }).to_string(),
                "meta": {
                    "apiFormat": "openai_chat"
                },
                "is_disabled": false
            }),
            None,
        )
        .unwrap()
        .unwrap();

        assert_eq!(result.target_protocol, AiProtocol::OpenAiChat);
        assert_eq!(result.auth_strategy, ProviderAuthStrategy::Bearer);
        assert_eq!(
            result.base_url,
            "https://openrouter.example.com/v1/chat/completions"
        );
        assert!(result.is_full_url);
    }

    #[test]
    fn provider_meta_reads_reasoning_field_policy() {
        let result = provider_from_record(
            GatewayCliKey::Codex,
            serde_json::json!({
                "id": "codex-openrouter",
                "name": "Codex OpenRouter",
                "category": "custom",
                "settings_config": serde_json::json!({
                    "auth": {"OPENAI_API_KEY": "openrouter-key"},
                    "config": r#"
model_provider = "custom"
[model_providers.custom]
base_url = "https://openrouter.ai/api/v1"
"#
                }).to_string(),
                "meta": {
                    "providerType": "openrouter",
                    "apiFormat": "openai_chat",
                    "reasoningField": "reasoning"
                },
                "is_disabled": false
            }),
            None,
        )
        .unwrap()
        .unwrap();

        assert_eq!(result.meta.provider_type.as_deref(), Some("openrouter"));
        assert_eq!(result.target_protocol, AiProtocol::OpenAiChat);
        assert_eq!(result.meta.reasoning_field.as_deref(), Some("reasoning"));
    }

    #[test]
    fn provider_meta_reads_codex_chat_reasoning_config() {
        let result = provider_from_record(
            GatewayCliKey::Codex,
            serde_json::json!({
                "id": "codex-deepseek",
                "name": "Codex DeepSeek",
                "category": "custom",
                "settings_config": serde_json::json!({
                    "auth": {"OPENAI_API_KEY": "deepseek-key"},
                    "config": r#"
model_provider = "custom"
[model_providers.custom]
base_url = "https://api.deepseek.com/v1"
"#
                }).to_string(),
                "meta": {
                    "providerType": "deepseek",
                    "apiFormat": "openai_chat",
                    "codexChatReasoning": {
                        "supportsThinking": true,
                        "supportsEffort": true,
                        "thinkingParam": "thinking",
                        "effortParam": "reasoning_effort",
                        "effortValueMode": "deepseek",
                        "outputFormat": "reasoning_content"
                    }
                },
                "is_disabled": false
            }),
            None,
        )
        .unwrap()
        .unwrap();

        let config = result
            .meta
            .codex_chat_reasoning
            .as_ref()
            .expect("codex chat reasoning config");
        assert_eq!(config.supports_thinking, Some(true));
        assert_eq!(config.supports_effort, Some(true));
        assert_eq!(config.thinking_param.as_deref(), Some("thinking"));
        assert_eq!(config.effort_param.as_deref(), Some("reasoning_effort"));
        assert_eq!(config.effort_value_mode.as_deref(), Some("deepseek"));
        assert_eq!(config.output_format.as_deref(), Some("reasoning_content"));
    }

    #[test]
    fn provider_meta_extracts_model_catalog_image_capabilities() {
        let result = provider_from_record(
            GatewayCliKey::Codex,
            serde_json::json!({
                "id": "codex-catalog",
                "name": "Codex Catalog",
                "category": "custom",
                "settings_config": serde_json::json!({
                    "auth": {"OPENAI_API_KEY": "catalog-key"},
                    "config": r#"
model_provider = "custom"
[model_providers.custom]
base_url = "https://api.example.com/v1"
"#,
                    "modelCatalog": {
                        "models": [
                            {
                                "model": "text-only-model",
                                "modalities": {"input": ["text"], "output": ["text"]}
                            },
                            {
                                "model": "vision-model",
                                "modalities": {"input": ["text", "image"], "output": ["text"]}
                            }
                        ]
                    }
                }).to_string(),
                "meta": {
                    "apiFormat": "openai_chat"
                },
                "is_disabled": false
            }),
            None,
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            result.meta.text_only_models,
            vec!["text-only-model".to_string()]
        );
        assert_eq!(
            result.meta.image_capable_models,
            vec!["vision-model".to_string()]
        );
    }

    #[test]
    fn provider_meta_reads_model_rewrites_and_filters_blank_rules() {
        let meta = provider_meta_from_record(
            GatewayCliKey::Codex,
            &serde_json::json!({
                "category": "custom",
                "meta": {
                    "modelRewrites": [
                        { "from": "gpt-5-luna", "to": "gpt-5-mini" },
                        { "from": "", "to": "ignored" },
                        { "from": "x", "to": "  " }
                    ]
                }
            }),
            None,
        );

        assert_eq!(
            meta.model_rewrites,
            Some(vec![ModelRewriteRule {
                from: "gpt-5-luna".to_string(),
                to: "gpt-5-mini".to_string(),
            }])
        );
    }

    #[test]
    fn provider_meta_reads_snake_case_model_rewrites_and_defaults_to_none() {
        let meta = provider_meta_from_record(
            GatewayCliKey::Gemini,
            &serde_json::json!({
                "category": "custom",
                "meta": {
                    "model_rewrites": [
                        { "from": "gemini-2.5-flash-lite", "to": "gemini-2.5-flash" }
                    ]
                }
            }),
            None,
        );
        assert_eq!(
            meta.model_rewrites,
            Some(vec![ModelRewriteRule {
                from: "gemini-2.5-flash-lite".to_string(),
                to: "gemini-2.5-flash".to_string(),
            }])
        );

        let empty = provider_meta_from_record(
            GatewayCliKey::Gemini,
            &serde_json::json!({ "category": "custom", "meta": {} }),
            None,
        );
        assert_eq!(empty.model_rewrites, None);
    }

    #[test]
    fn provider_meta_resolves_gateway_profile_reference_from_catalog() {
        let meta = provider_meta_from_record(
            GatewayCliKey::Codex,
            &serde_json::json!({
                "category": "custom",
                "meta": {
                    "gatewayProfile": {
                        "tool": "codex",
                        "profileId": "deepseek",
                        "endpointId": "openai_chat"
                    }
                }
            }),
            None,
        );

        assert_eq!(meta.provider_type.as_deref(), Some("deepseek"));
        assert_eq!(meta.api_format.as_deref(), Some("openai_chat"));
        assert_eq!(
            meta.codex_chat_reasoning
                .as_ref()
                .and_then(|config| config.effort_value_mode.as_deref()),
            Some("deepseek")
        );
    }

    #[test]
    fn provider_meta_gateway_profile_reference_preserves_user_owned_fields() {
        let meta = provider_meta_from_record(
            GatewayCliKey::Claude,
            &serde_json::json!({
                "category": "custom",
                "meta": {
                    "gatewayProfile": {
                        "tool": "claude",
                        "profileId": "deepseek",
                        "endpointId": "openai_chat"
                    },
                    "costMultiplier": "1.25",
                    "pricingModelSource": "requested",
                    "isFullUrl": true,
                    "promptCacheKey": "session-key"
                }
            }),
            None,
        );

        assert_eq!(meta.provider_type.as_deref(), Some("deepseek"));
        assert_eq!(meta.api_format.as_deref(), Some("openai_chat"));
        assert_eq!(meta.cost_multiplier, "1.25");
        assert_eq!(meta.pricing_model_source, "requested");
        assert!(meta.is_full_url);
        assert_eq!(meta.prompt_cache_key.as_deref(), Some("session-key"));
    }

    #[test]
    fn provider_meta_gateway_profile_reference_uses_current_profile_fields() {
        let meta = provider_meta_from_record(
            GatewayCliKey::Gemini,
            &serde_json::json!({
                "category": "custom",
                "meta": {
                    "gatewayProfile": {
                        "tool": "gemini",
                        "profileId": "openrouter",
                        "endpointId": "openai_chat"
                    }
                }
            }),
            None,
        );

        assert_eq!(meta.provider_type.as_deref(), Some("openrouter"));
        assert_eq!(meta.api_format.as_deref(), Some("openai_chat"));
        assert_eq!(meta.reasoning_field.as_deref(), Some("reasoning"));
    }

    #[test]
    fn provider_meta_gateway_profile_reference_does_not_apply_codex_reasoning_for_gemini() {
        let meta = provider_meta_from_record(
            GatewayCliKey::Gemini,
            &serde_json::json!({
                "category": "custom",
                "meta": {
                    "gatewayProfile": {
                        "tool": "gemini",
                        "profileId": "openrouter",
                        "endpointId": "openai_chat"
                    },
                    "codexChatReasoning": {
                        "supportsEffort": true,
                        "effortValueMode": "legacy"
                    }
                }
            }),
            None,
        );

        assert_eq!(meta.provider_type.as_deref(), Some("openrouter"));
        assert!(meta.codex_chat_reasoning.is_none());
    }

    #[test]
    fn provider_meta_gateway_profile_reference_falls_back_to_legacy_when_missing() {
        let meta = provider_meta_from_record(
            GatewayCliKey::Codex,
            &serde_json::json!({
                "category": "custom",
                "meta": {
                    "gatewayProfile": {
                        "tool": "codex",
                        "profileId": "missing-profile",
                        "endpointId": "openai_chat"
                    },
                    "providerType": "legacy-provider",
                    "apiFormat": "openai_chat"
                }
            }),
            None,
        );

        assert_eq!(meta.provider_type.as_deref(), Some("legacy-provider"));
        assert_eq!(meta.api_format.as_deref(), Some("openai_chat"));
    }

    #[test]
    fn codex_api_format_meta_selects_anthropic_target() {
        let result = provider_from_record(
            GatewayCliKey::Codex,
            serde_json::json!({
                "id": "codex-anthropic",
                "name": "Codex Anthropic",
                "category": "custom",
                "settings_config": serde_json::json!({
                    "auth": {"OPENAI_API_KEY": "anthropic-key"},
                    "config": r#"
model_provider = "custom"
[model_providers.custom]
base_url = "https://api.anthropic.com"
"#
                }).to_string(),
                "meta": {
                    "apiFormat": "anthropic_messages",
                    "apiKeyField": "x-api-key"
                },
                "is_disabled": false
            }),
            None,
        )
        .unwrap()
        .unwrap();

        assert_eq!(result.target_protocol, AiProtocol::AnthropicMessages);
        assert_eq!(result.auth_strategy, ProviderAuthStrategy::AnthropicApiKey);
    }

    #[test]
    fn codex_slash_api_format_meta_selects_anthropic_target() {
        let result = provider_from_record(
            GatewayCliKey::Codex,
            serde_json::json!({
                "id": "codex-anthropic-slash",
                "name": "Codex Anthropic Slash",
                "category": "custom",
                "settings_config": serde_json::json!({
                    "auth": {"OPENAI_API_KEY": "anthropic-key"},
                    "config": r#"
model_provider = "custom"
[model_providers.custom]
base_url = "https://api.anthropic.com"
"#
                }).to_string(),
                "meta": {
                    "apiFormat": "anthropic/messages",
                    "apiKeyField": "x-api-key"
                },
                "is_disabled": false
            }),
            None,
        )
        .unwrap()
        .unwrap();

        assert_eq!(result.target_protocol, AiProtocol::AnthropicMessages);
        assert_eq!(result.auth_strategy, ProviderAuthStrategy::AnthropicApiKey);
    }

    #[test]
    fn gemini_api_format_meta_selects_anthropic_target_and_api_key_auth() {
        let result = provider_from_record(
            GatewayCliKey::Gemini,
            serde_json::json!({
                "id": "gemini-anthropic",
                "name": "Gemini Anthropic",
                "category": "custom",
                "settings_config": serde_json::json!({
                    "env": {
                        "GOOGLE_GEMINI_BASE_URL": "https://api.anthropic.com",
                        "GEMINI_API_KEY": "anthropic-key"
                    }
                }).to_string(),
                "meta": {
                    "apiFormat": "anthropic"
                },
                "is_disabled": false
            }),
            None,
        )
        .unwrap()
        .unwrap();

        assert_eq!(result.target_protocol, AiProtocol::AnthropicMessages);
        assert_eq!(result.auth_strategy, ProviderAuthStrategy::AnthropicApiKey);
        assert_eq!(result.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn gemini_api_format_meta_selects_anthropic_bearer_for_auth_token() {
        let result = provider_from_record(
            GatewayCliKey::Gemini,
            serde_json::json!({
                "id": "gemini-anthropic-token",
                "name": "Gemini Anthropic Token",
                "category": "custom",
                "settings_config": serde_json::json!({
                    "env": {
                        "GOOGLE_GEMINI_BASE_URL": "https://api.anthropic.com",
                        "ANTHROPIC_AUTH_TOKEN": "token"
                    }
                }).to_string(),
                "meta": {
                    "apiFormat": "anthropic"
                },
                "is_disabled": false
            }),
            None,
        )
        .unwrap()
        .unwrap();

        assert_eq!(result.target_protocol, AiProtocol::AnthropicMessages);
        assert_eq!(result.auth_strategy, ProviderAuthStrategy::Bearer);
    }

    #[test]
    fn gemini_provider_defaults_to_native_protocol_and_google_auth() {
        let result = provider_from_record(
            GatewayCliKey::Gemini,
            serde_json::json!({
                "id": "gemini-native",
                "name": "Gemini Native",
                "category": "custom",
                "settings_config": serde_json::json!({
                    "env": {
                        "GOOGLE_GEMINI_BASE_URL": "https://generativelanguage.googleapis.com/v1beta",
                        "GEMINI_API_KEY": "google-key"
                    }
                }).to_string(),
                "is_disabled": false
            }),
            None,
        )
        .unwrap()
        .unwrap();

        assert_eq!(result.target_protocol, AiProtocol::GeminiNative);
        assert_eq!(result.auth_strategy, ProviderAuthStrategy::GoogleApiKey);
    }
}
