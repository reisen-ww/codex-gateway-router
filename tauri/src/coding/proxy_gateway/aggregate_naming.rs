//! Aggregate-mode model naming rules.
//!
//! Aggregate mode exposes one Codex model list built from several sites. A site
//! is addressed by a *prefix token*: the user's explicit alias when configured,
//! otherwise the provider's configured display name when it is safe to use, and
//! finally the auto-generated provider id (`76a6ef74`, `ccex`, …). Three templates are
//! supported:
//!
//! - `site_model` (default, the historical behaviour): `<prefix><sep><model>`
//! - `model_at_site`: `<model><sep><prefix>`
//! - `model_only`: `<model>` with a `#N` disambiguator when several sites declare
//!   the same upstream model.
//!
//! Design note (why `model_only` auto-numbers instead of rejecting duplicates):
//! sharing a model name across sites is the normal case for the aggregate mode
//! this feature exists for (several relays reselling the same upstream models).
//! Rejecting the combination would make the template unusable for exactly the
//! users who need it, and the failure would only surface at engage time. A
//! deterministic `#N` suffix keeps every slug addressable while remaining
//! trivially reproducible: both the Codex catalog and the request-time router
//! build the same table by walking the same ordered `(site, model)` pairs, so a
//! generated slug like `deepseek-v4-flash#2` always resolves back to the second
//! site that declared `deepseek-v4-flash`.
//!
//! Invariants:
//! - Alias charset is Unicode letters/digits plus ASCII spaces, `_`, and `-`.
//!   The separator rejects that entire charset, so `<prefix><sep><model>` can
//!   always be split back without treating a human-readable site name as model
//!   syntax.
//! - Aliases are unique case-insensitively because aggregate prefix matching is
//!   case-insensitive; the same applies to the effective prefix token (alias,
//!   else provider id) of every site.
//! - Slugs are globally unique. Collisions are never resolved by silently
//!   overwriting an earlier entry: `model_only` appends `#N`, and any residual
//!   collision returns an actionable error.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Maximum alias length accepted by the backend and the settings page.
pub const AGGREGATE_ALIAS_MAX_LEN: usize = 32;

/// How a `(site, model)` pair is turned into the model slug Codex sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AggregateNamingMode {
    /// `<prefix><separator><model>` — the historical aggregate slug shape.
    #[default]
    SiteModel,
    /// `<model><separator><prefix>`.
    ModelAtSite,
    /// `<model>` only; duplicates get a `#N` suffix.
    ModelOnly,
}

impl AggregateNamingMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SiteModel => "site_model",
            Self::ModelAtSite => "model_at_site",
            Self::ModelOnly => "model_only",
        }
    }

    /// Whether this template joins the model name with the site prefix.
    ///
    /// `separator` is the connector for the two prefix templates and is unused
    /// by `model_only` (there is nothing to join). Keeping one connector for
    /// both prefix templates avoids a second user-facing field and mirrors the
    /// pre-existing `separator` manifest key.
    pub fn uses_separator(self) -> bool {
        match self {
            Self::SiteModel | Self::ModelAtSite => true,
            Self::ModelOnly => false,
        }
    }
}

/// Effective naming configuration shared by catalog generation and routing.
///
/// Both sides must build slugs from this struct only: the `model_only` template
/// numbers duplicate models by walk order, so any divergence between the two
/// walk orders would silently route a `#N` slug to the wrong site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateNamingConfig {
    /// Connector between prefix and model name (unused by `model_only`).
    pub separator: String,
    /// provider id -> explicit or display-name-derived site prefix. Missing/blank
    /// entries fall back to the provider id.
    pub aliases: BTreeMap<String, String>,
    pub naming: AggregateNamingMode,
}

impl Default for AggregateNamingConfig {
    fn default() -> Self {
        Self {
            separator: ".".to_string(),
            aliases: BTreeMap::new(),
            naming: AggregateNamingMode::SiteModel,
        }
    }
}

impl AggregateNamingConfig {
    /// Prefix token that addresses one site: its resolved alias, else its
    /// provider id.
    pub fn prefix_for(&self, site_id: &str) -> String {
        aggregate_site_prefix(site_id, &self.aliases).to_string()
    }

    /// Allocate one slug using this configuration.
    pub fn allocate(
        &self,
        allocator: &mut AggregateSlugAllocator,
        site_id: &str,
        model: &str,
    ) -> Result<Option<String>, String> {
        allocator.allocate(
            site_id,
            model,
            &self.separator,
            self.naming,
            &self.prefix_for(site_id),
        )
    }
}

/// Normalize a prefix for case-insensitive uniqueness checks and route matching.
fn aggregate_prefix_key(value: &str) -> String {
    value.to_lowercase()
}

fn aggregate_prefix_matches(left: &str, right: &str) -> bool {
    aggregate_prefix_key(left) == aggregate_prefix_key(right)
}

/// Validate one configured alias.
///
/// An empty string means "no alias configured" and must be filtered out by the
/// caller before validating; reaching this function with an empty value is a
/// programming error the caller surfaces as a validation failure.
pub fn validate_aggregate_alias(alias: &str) -> Result<(), String> {
    if alias.is_empty() {
        return Err("Aggregate site alias must not be empty".to_string());
    }
    if alias.chars().count() > AGGREGATE_ALIAS_MAX_LEN {
        return Err(format!(
            "Aggregate site alias must be at most {AGGREGATE_ALIAS_MAX_LEN} characters"
        ));
    }
    if alias
        .chars()
        .any(|ch| !(ch.is_alphanumeric() || ch == '_' || ch == '-' || ch == ' '))
    {
        return Err(
            "Aggregate site alias may only contain letters, digits, spaces, '_' or '-'".to_string(),
        );
    }
    Ok(())
}

/// Validate every configured alias: charset/length plus case-insensitive
/// uniqueness, because prefix matching is case-insensitive.
pub fn validate_aggregate_aliases(aliases: &BTreeMap<String, String>) -> Result<(), String> {
    let mut seen: BTreeMap<String, &str> = BTreeMap::new();
    for (provider_id, alias) in aliases {
        let alias = alias.trim();
        if alias.is_empty() {
            // Blank entries are dropped before validation; treat a stored blank
            // as invalid so a hand-edited manifest cannot silently change names.
            return Err(format!(
                "Aggregate site alias for '{provider_id}' must not be empty"
            ));
        }
        validate_aggregate_alias(alias)
            .map_err(|error| format!("Aggregate site alias '{alias}': {error}"))?;
        let key = aggregate_prefix_key(alias);
        if let Some(previous) = seen.get(&key) {
            return Err(format!(
                "Aggregate site alias '{alias}' is used by more than one site ('{previous}' and '{provider_id}'); aliases must be globally unique"
            ));
        }
        seen.insert(key, provider_id);
    }
    Ok(())
}

/// Return a provider display name when it is safe to use as an aggregate prefix.
///
/// This is intentionally conservative: punctuation is excluded so a user-picked
/// separator cannot become part of the prefix. Callers may still preserve the
/// opaque provider id for names that are not safe or not unique.
pub fn default_aggregate_site_alias(site_label: &str) -> Option<String> {
    let alias = site_label.trim();
    validate_aggregate_alias(alias)
        .ok()
        .map(|_| alias.to_string())
}

/// Resolve the legacy aggregate prefix map before it is persisted in the
/// manifest.
///
/// Explicit aliases win. For an otherwise unaliased selected provider, use its
/// configured display name when the name is safe, unique, and cannot shadow an
/// addressable provider id. This keeps generated model names human-readable
/// (for example `思辰888.gpt-5`) while retaining the provider id as the
/// deterministic fallback for collisions or unsafe names.
pub fn resolve_aggregate_site_aliases(
    supplied_aliases: BTreeMap<String, String>,
    selected_sites: &[(String, String)],
    addressable_site_ids: &[String],
) -> Result<BTreeMap<String, String>, String> {
    let selected_ids = selected_sites
        .iter()
        .map(|(site_id, _)| site_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut aliases = BTreeMap::new();

    for (raw_site_id, raw_alias) in supplied_aliases {
        let site_id = raw_site_id.trim();
        let alias = raw_alias.trim();
        if alias.is_empty() {
            continue;
        }
        if !selected_ids.contains(site_id) {
            return Err(format!(
                "Aggregate alias references unselected site '{raw_site_id}'"
            ));
        }
        aliases.insert(site_id.to_string(), alias.to_string());
    }

    validate_aggregate_aliases(&aliases)?;

    let explicit_prefixes = aliases
        .values()
        .map(|alias| aggregate_prefix_key(alias))
        .collect::<BTreeSet<_>>();
    let addressable_id_keys = addressable_site_ids
        .iter()
        .map(|site_id| aggregate_prefix_key(site_id))
        .collect::<BTreeSet<_>>();
    let mut defaults_by_prefix = BTreeMap::<String, Vec<(String, String)>>::new();

    for (site_id, site_label) in selected_sites {
        if aliases.contains_key(site_id) {
            continue;
        }
        let Some(alias) = default_aggregate_site_alias(site_label) else {
            continue;
        };
        let key = aggregate_prefix_key(&alias);
        // An explicit alias wins. A derived display name that looks like another
        // provider id would make the compatibility provider-id prefix ambiguous,
        // so leave that provider on its id fallback instead.
        if explicit_prefixes.contains(&key)
            || (addressable_id_keys.contains(&key) && !site_id.eq_ignore_ascii_case(alias.as_str()))
        {
            continue;
        }
        defaults_by_prefix
            .entry(key)
            .or_default()
            .push((site_id.clone(), alias));
    }

    // If two user-configured provider names are the same, neither silently wins:
    // each retains the stable provider-id fallback until the user sets an
    // explicit alias.
    for candidates in defaults_by_prefix.into_values() {
        if candidates.len() == 1 {
            let (site_id, alias) = candidates.into_iter().next().expect("one candidate");
            aliases.insert(site_id, alias);
        }
    }

    validate_aggregate_aliases(&aliases)?;
    validate_aggregate_site_prefixes(addressable_site_ids, &aliases)?;
    Ok(aliases)
}

/// The prefix token that addresses one site: its alias, else its provider id.
pub fn aggregate_site_prefix<'a>(
    site_id: &'a str,
    aliases: &'a BTreeMap<String, String>,
) -> &'a str {
    aliases
        .get(site_id)
        .map(|alias| alias.trim())
        .filter(|alias| !alias.is_empty())
        .unwrap_or(site_id)
}

/// Validate that every site in `site_ids` has a distinct effective prefix.
///
/// `site_ids` must contain every addressable site (selected and unselected),
/// because unselected sites stay available as aggregate fallbacks and their ids
/// are still resolvable when a request carries a prefix.
pub fn validate_aggregate_site_prefixes(
    site_ids: &[String],
    aliases: &BTreeMap<String, String>,
) -> Result<(), String> {
    let provider_ids_by_key = site_ids
        .iter()
        .map(|site_id| (aggregate_prefix_key(site_id), site_id.as_str()))
        .collect::<BTreeMap<_, _>>();

    // The runtime intentionally keeps provider-id prefixes working for backward
    // compatibility even after an alias is selected. Therefore an alias cannot
    // reuse another provider's id, including an id belonging to a selected site.
    for (provider_id, alias) in aliases {
        let alias_key = aggregate_prefix_key(alias.trim());
        if let Some(conflicting_provider_id) = provider_ids_by_key.get(&alias_key) {
            if !provider_id.eq_ignore_ascii_case(conflicting_provider_id) {
                return Err(format!(
                    "Aggregate site alias '{alias}' for '{provider_id}' conflicts with provider id '{conflicting_provider_id}'; choose a different alias"
                ));
            }
        }
    }

    let mut seen: BTreeMap<String, &str> = BTreeMap::new();
    for site_id in site_ids {
        let prefix = aggregate_site_prefix(site_id, aliases);
        if prefix.is_empty() {
            continue;
        }
        let key = aggregate_prefix_key(prefix);
        if let Some(previous) = seen.get(&key) {
            return Err(format!(
                "Aggregate site name '{prefix}' addresses both '{previous}' and '{site_id}'; site aliases must be unique across every site"
            ));
        }
        seen.insert(key, site_id);
    }
    Ok(())
}

/// One `(site, model)` pair with the slug it must be published under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregateSlugEntry {
    pub site_id: String,
    pub upstream_model: String,
    pub slug: String,
}

/// Allocates slugs for `(site, model)` pairs in iteration order.
///
/// Both the Codex catalog generator and the request-time router walk the same
/// ordered pairs (selected sites in display order, then each site's declared
/// models in declaration order), which is what makes `#N` numbering stable.
#[derive(Debug, Default)]
pub struct AggregateSlugAllocator {
    pairs: BTreeSet<(String, String)>,
    slugs: BTreeSet<String>,
}

impl AggregateSlugAllocator {
    /// Allocate the slug for one pair.
    ///
    /// Returns `Ok(None)` when this exact `(site, model)` pair was already
    /// emitted, so duplicated catalog rows collapse instead of producing a
    /// second entry. Two *different* pairs never share a slug: `model_only`
    /// appends `#N`, and every other mode reports the conflict as an error
    /// instead of overwriting the earlier entry.
    pub fn allocate(
        &mut self,
        site_id: &str,
        model: &str,
        separator: &str,
        naming: AggregateNamingMode,
        prefix: &str,
    ) -> Result<Option<String>, String> {
        let pair = (site_id.to_string(), model.to_string());
        if self.pairs.contains(&pair) {
            return Ok(None);
        }
        let candidate = match naming {
            AggregateNamingMode::SiteModel => format!("{prefix}{separator}{model}"),
            AggregateNamingMode::ModelAtSite => format!("{model}{separator}{prefix}"),
            AggregateNamingMode::ModelOnly => model.to_string(),
        };
        let slug = match naming {
            AggregateNamingMode::ModelOnly => {
                let mut slug = candidate.clone();
                let mut index = 2_u32;
                while self.slugs.contains(&slug) {
                    slug = format!("{candidate}#{index}");
                    index += 1;
                }
                slug
            }
            _ => {
                if self.slugs.contains(&candidate) {
                    return Err(format!(
                        "Aggregate model name '{candidate}' would be produced by more than one site; adjust the site aliases or the naming mode"
                    ));
                }
                candidate
            }
        };
        self.pairs.insert(pair);
        self.slugs.insert(slug.clone());
        Ok(Some(slug))
    }
}

/// Build the full slug table for an ordered list of `(site_id, models)`.
pub fn build_aggregate_slug_table(
    sites: &[(String, Vec<String>)],
    separator: &str,
    aliases: &BTreeMap<String, String>,
    naming: AggregateNamingMode,
) -> Result<Vec<AggregateSlugEntry>, String> {
    let mut allocator = AggregateSlugAllocator::default();
    let mut entries = Vec::new();
    for (site_id, models) in sites {
        if site_id.trim().is_empty() {
            continue;
        }
        let prefix = aggregate_site_prefix(site_id, aliases);
        for model in models {
            let model = model.trim();
            if model.is_empty() {
                continue;
            }
            let Some(slug) = allocator.allocate(site_id, model, separator, naming, prefix)? else {
                continue;
            };
            entries.push(AggregateSlugEntry {
                site_id: site_id.clone(),
                upstream_model: model.to_string(),
                slug,
            });
        }
    }
    Ok(entries)
}

/// Split `<prefix><separator><model>` (site-model template) into its parts.
///
/// Only the configured separator after a known prefix token is stripped, so
/// dots/separators inside the upstream model name survive untouched.
pub fn split_site_model_slug<'a>(
    requested_model: &'a str,
    separator: &str,
    sites: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<(String, String)> {
    if separator.is_empty() {
        return None;
    }
    for (site_id, prefix) in sites {
        if prefix.is_empty() {
            continue;
        }
        let prefix_len = prefix.len() + separator.len();
        if requested_model.len() <= prefix_len {
            continue;
        }
        // Client input is not guaranteed to use the same UTF-8 byte length as
        // a configured Unicode prefix. Never split in the middle of an
        // arbitrary request character merely to reject a non-matching prefix.
        if !requested_model.is_char_boundary(prefix.len()) {
            continue;
        }
        let (head, rest) = requested_model.split_at(prefix.len());
        if !aggregate_prefix_matches(head, prefix) {
            continue;
        }
        let Some(model) = rest.strip_prefix(separator) else {
            continue;
        };
        let model = model.trim();
        if model.is_empty() {
            continue;
        }
        return Some((site_id.to_string(), model.to_string()));
    }
    None
}

/// Split `<model><separator><prefix>` (model-at-site template) into its parts.
///
/// The prefix token can never contain the separator, so the suffix match at the
/// end of the string is unambiguous.
pub fn split_model_at_site_slug<'a>(
    requested_model: &'a str,
    separator: &str,
    sites: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<(String, String)> {
    if separator.is_empty() {
        return None;
    }
    for (site_id, prefix) in sites {
        if prefix.is_empty() {
            continue;
        }
        let suffix_len = prefix.len() + separator.len();
        if requested_model.len() <= suffix_len {
            continue;
        }
        let split_at = requested_model.len() - suffix_len;
        // A request can use arbitrary Unicode even when the configured suffix
        // does not match. Guard the calculated byte offset before slicing.
        if !requested_model.is_char_boundary(split_at) {
            continue;
        }
        let (model, tail) = requested_model.split_at(split_at);
        if !aggregate_prefix_matches(tail, &format!("{separator}{prefix}")) {
            continue;
        }
        let model = model.trim();
        if model.is_empty() {
            continue;
        }
        return Some((site_id.to_string(), model.to_string()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aliases(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(id, alias)| (id.to_string(), alias.to_string()))
            .collect()
    }

    fn sites(pairs: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
        pairs
            .iter()
            .map(|(id, models)| {
                (
                    id.to_string(),
                    models.iter().map(|model| model.to_string()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn alias_validation_accepts_safe_site_name_charset() {
        assert!(validate_aggregate_alias("unsee").is_ok());
        assert!(validate_aggregate_alias("chain888").is_ok());
        assert!(validate_aggregate_alias("my-site_1").is_ok());
        assert!(validate_aggregate_alias("with space").is_ok());
        assert!(validate_aggregate_alias("思辰888").is_ok());
        assert!(validate_aggregate_alias("思源888 pro").is_ok());
        assert!(validate_aggregate_alias(&"a".repeat(AGGREGATE_ALIAS_MAX_LEN)).is_ok());

        assert!(validate_aggregate_alias("").is_err());
        assert!(validate_aggregate_alias(&"a".repeat(AGGREGATE_ALIAS_MAX_LEN + 1)).is_err());
        assert!(validate_aggregate_alias("with.dot").is_err());
        assert!(validate_aggregate_alias("with:colon").is_err());
    }

    #[test]
    fn alias_validation_rejects_case_insensitive_duplicates() {
        assert!(validate_aggregate_aliases(&aliases(&[("a", "unsee"), ("b", "chain888")])).is_ok());
        assert!(validate_aggregate_aliases(&aliases(&[("a", "unsee"), ("b", "UNSEE")])).is_err());
        assert!(validate_aggregate_aliases(&aliases(&[("a", "unsee"), ("b", "unsee")])).is_err());
        assert!(validate_aggregate_aliases(&aliases(&[("a", "")])).is_err());
    }

    #[test]
    fn site_prefixes_prefer_alias_and_must_stay_unique() {
        let with_alias = aliases(&[("76a6ef74", "unsee")]);
        assert_eq!(aggregate_site_prefix("76a6ef74", &with_alias), "unsee");
        assert_eq!(aggregate_site_prefix("ccex", &with_alias), "ccex");

        // An alias may not shadow another site's id: both sites would answer to
        // the same prefix and the request could not be routed deterministically.
        let collapsed = aliases(&[("76a6ef74", "ccex")]);
        assert!(validate_aggregate_site_prefixes(
            &["76a6ef74".to_string(), "ccex".to_string()],
            &collapsed
        )
        .is_err());
        assert!(validate_aggregate_site_prefixes(
            &["76a6ef74".to_string(), "ccex".to_string()],
            &with_alias
        )
        .is_ok());
    }

    #[test]
    fn alias_cannot_shadow_an_unselected_enabled_provider_id() {
        let selected = vec!["site-a".to_string()];
        let all_enabled = vec!["site-a".to_string(), "site-b".to_string()];
        let aliases = aliases(&[("site-a", "site-b")]);

        assert!(validate_aggregate_site_prefixes(&selected, &aliases).is_ok());
        assert!(validate_aggregate_site_prefixes(&all_enabled, &aliases).is_err());
    }

    #[test]
    fn resolver_defaults_to_unique_safe_provider_names_and_keeps_explicit_aliases() {
        let selected_sites = vec![
            (
                "76a6ef74af6c4151812787cc519b534b".to_string(),
                "思辰888".to_string(),
            ),
            ("site-b".to_string(), "思源888 pro".to_string()),
            ("site-c".to_string(), "思源888".to_string()),
            ("site-d".to_string(), "unsafe.name".to_string()),
        ];
        let addressable_site_ids = selected_sites
            .iter()
            .map(|(site_id, _)| site_id.clone())
            .collect::<Vec<_>>();

        let resolved = resolve_aggregate_site_aliases(
            aliases(&[("site-b", "manual-name")]),
            &selected_sites,
            &addressable_site_ids,
        )
        .unwrap();

        // The UUID uses the configured provider name. Explicit aliases still
        // take precedence, while an unsafe display name falls back to the
        // stable provider id.
        assert_eq!(
            resolved.get("76a6ef74af6c4151812787cc519b534b"),
            Some(&"思辰888".to_string())
        );
        assert_eq!(resolved.get("site-b"), Some(&"manual-name".to_string()));
        assert_eq!(resolved.get("site-c"), Some(&"思源888".to_string()));
        assert!(!resolved.contains_key("site-d"));

        let table = build_aggregate_slug_table(
            &sites(&[("76a6ef74af6c4151812787cc519b534b", &["deepseek-v4.1-flash"])]),
            ".",
            &resolved,
            AggregateNamingMode::SiteModel,
        )
        .unwrap();
        assert_eq!(table[0].slug, "思辰888.deepseek-v4.1-flash");
    }

    #[test]
    fn slug_table_names_every_mode() {
        let table = build_aggregate_slug_table(
            &sites(&[("76a6ef74", &["deepseek-v4-flash"])]),
            ".",
            &aliases(&[("76a6ef74", "unsee")]),
            AggregateNamingMode::SiteModel,
        )
        .unwrap();
        assert_eq!(table[0].slug, "unsee.deepseek-v4-flash");
        assert_eq!(table[0].upstream_model, "deepseek-v4-flash");

        let table = build_aggregate_slug_table(
            &sites(&[("76a6ef74", &["deepseek-v4-flash"])]),
            "@",
            &aliases(&[("76a6ef74", "unsee")]),
            AggregateNamingMode::ModelAtSite,
        )
        .unwrap();
        assert_eq!(table[0].slug, "deepseek-v4-flash@unsee");

        let table = build_aggregate_slug_table(
            &sites(&[("76a6ef74", &["deepseek-v4-flash"])]),
            ".",
            &BTreeMap::new(),
            AggregateNamingMode::ModelOnly,
        )
        .unwrap();
        assert_eq!(table[0].slug, "deepseek-v4-flash");
    }

    #[test]
    fn model_only_numbers_duplicate_models_in_site_order() {
        let table = build_aggregate_slug_table(
            &sites(&[
                ("site-a", &["deepseek-v4-flash", "glm-5"]),
                ("site-b", &["deepseek-v4-flash"]),
                ("site-c", &["deepseek-v4-flash"]),
            ]),
            ".",
            &BTreeMap::new(),
            AggregateNamingMode::ModelOnly,
        )
        .unwrap();

        let slugs: Vec<&str> = table.iter().map(|entry| entry.slug.as_str()).collect();
        assert_eq!(
            slugs,
            vec![
                "deepseek-v4-flash",
                "glm-5",
                "deepseek-v4-flash#2",
                "deepseek-v4-flash#3"
            ]
        );
        // Numbering keeps pointing at the original upstream model name.
        assert_eq!(table[2].site_id, "site-b");
        assert_eq!(table[2].upstream_model, "deepseek-v4-flash");
    }

    #[test]
    fn slug_table_dedupes_repeated_pairs_and_reports_prefix_collisions() {
        let table = build_aggregate_slug_table(
            &sites(&[("site-a", &["m", "m"])]),
            ".",
            &BTreeMap::new(),
            AggregateNamingMode::SiteModel,
        )
        .unwrap();
        assert_eq!(table.len(), 1);

        // Two prefixes that differ only by the separator can never collide in
        // the prefix modes, so this can only be produced by handing the
        // allocator the same prefix twice — the guard must error, never
        // overwrite the first entry.
        let mut allocator = AggregateSlugAllocator::default();
        allocator
            .allocate("site-a", "m", ".", AggregateNamingMode::SiteModel, "same")
            .unwrap();
        assert!(allocator
            .allocate("site-b", "m", ".", AggregateNamingMode::SiteModel, "same")
            .is_err());
    }

    #[test]
    fn split_helpers_match_only_their_own_template() {
        let entries = [("site-a", "unsee"), ("site-b", "chain888")];

        assert_eq!(
            split_site_model_slug("unsee.deepseek-v4.1-flash", ".", entries),
            Some(("site-a".to_string(), "deepseek-v4.1-flash".to_string()))
        );
        assert_eq!(
            split_site_model_slug("deepseek-v4-flash", ".", entries),
            None
        );
        assert_eq!(split_site_model_slug("unsee", ".", entries), None);
        assert_eq!(split_site_model_slug("unsee.", ".", entries), None);

        assert_eq!(
            split_model_at_site_slug("deepseek-v4-flash@chain888", "@", entries),
            Some(("site-b".to_string(), "deepseek-v4-flash".to_string()))
        );
        // A model name that itself contains the separator stays intact.
        assert_eq!(
            split_model_at_site_slug("deepseek-v4@1-flash@unsee", "@", entries),
            Some(("site-a".to_string(), "deepseek-v4@1-flash".to_string()))
        );
        assert_eq!(
            split_model_at_site_slug("deepseek-v4-flash", "@", entries),
            None
        );
    }

    #[test]
    fn split_helpers_are_case_insensitive_but_keep_model_case() {
        assert_eq!(
            split_site_model_slug("UNSEE.DeepSeek-V4", ".", [("site-a", "unsee")]),
            Some(("site-a".to_string(), "DeepSeek-V4".to_string()))
        );
        assert_eq!(
            split_model_at_site_slug("DeepSeek-V4@UNSEE", "@", [("site-a", "unsee")]),
            Some(("site-a".to_string(), "DeepSeek-V4".to_string()))
        );
        assert_eq!(
            split_site_model_slug(
                "思辰888.DeepSeek-V4",
                ".",
                [("76a6ef74af6c4151812787cc519b534b", "思辰888")],
            ),
            Some((
                "76a6ef74af6c4151812787cc519b534b".to_string(),
                "DeepSeek-V4".to_string(),
            ))
        );
    }

    #[test]
    fn split_helpers_reject_non_matching_unicode_without_invalid_utf8_slices() {
        // These inputs are not valid slugs for the configured ASCII prefix.
        // They must be rejected, not panic while calculating a byte offset
        // inside an arbitrary Unicode character from a client request.
        assert_eq!(
            split_site_model_slug("思.model", ".", [("site-a", "a")]),
            None
        );
        assert_eq!(
            split_model_at_site_slug(".思a", ".", [("site-a", "a")]),
            None
        );
    }
}
