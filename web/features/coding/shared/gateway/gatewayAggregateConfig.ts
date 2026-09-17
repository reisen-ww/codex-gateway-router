import type {
  GatewayAggregateGroup,
  GatewayAggregateNamingMode,
  GatewayCliTakeoverStatus,
} from '@/services';
import {
  isGatewayReengageMode,
  type GatewayAggregateReengageConfig,
} from './providerSaveReengage';
import { isGatewayAggregateMode } from './providerProtocol';

/**
 * Aggregate-mode helpers shared by the gateway settings panel and the provider
 * save/re-engage flow.
 *
 * Backend contract (mirrors `cli_proxy/manifest.rs`): a site id must match
 * `^[A-Za-z0-9_-]+$`, and the separator must be non-empty and must not contain
 * letters, digits, `_` or `-`, otherwise `<site_id><sep><model>` cannot be
 * split back into its parts. Keep this module free of i18n text so the callers
 * decide how to phrase the error.
 */

export type GatewayAggregateSeparatorInvalidReason = 'empty' | 'reservedCharacters';

const SITE_ID_PATTERN = /^[A-Za-z0-9_-]+$/;
const ALIAS_PATTERN = /^[A-Za-z0-9_-]+$/;
// Keep this utility executable from the Node test runner, which intentionally
// does not install Vite's `@/*` path alias. This mirrors the service contract.
const DEFAULT_AGGREGATE_SEPARATOR = '.';
export const AGGREGATE_ALIAS_MAX_LENGTH = 32;
export const AGGREGATE_GROUP_ID_MAX_LENGTH = 32;
const GROUP_ID_PATTERN = /^[A-Za-z0-9_-]+$/;

/** Site ids the backend can address; anything else must be dropped before engaging. */
export const isAggregateSiteId = (siteId: string): boolean =>
  SITE_ID_PATTERN.test(siteId.trim());

/** Match the backend command's trim + empty-value fallback semantics. */
export const normalizeGatewayAggregateSeparator = (separator: string): string =>
  separator.trim() || DEFAULT_AGGREGATE_SEPARATOR;

/**
 * Validate a user-supplied separator. Returns `null` when the separator is
 * usable; the caller maps the reason code to a localized message.
 */
export const validateGatewayAggregateSeparator = (
  separator: string,
): GatewayAggregateSeparatorInvalidReason | null => {
  const normalized = separator.trim();
  if (normalized.length === 0) {
    return 'empty';
  }
  if (/[A-Za-z0-9_-]/.test(normalized)) {
    return 'reservedCharacters';
  }
  return null;
};

export const validateGatewayAggregateAlias = (alias: string): boolean =>
  alias.length > 0 && alias.length <= AGGREGATE_ALIAS_MAX_LENGTH && ALIAS_PATTERN.test(alias);

/** Group ids are used as the strict `group.model` prefix. */
export const validateGatewayAggregateGroupId = (groupId: string): boolean => {
  const normalized = groupId.trim();
  return (
    normalized.length > 0 &&
    normalized.length <= AGGREGATE_GROUP_ID_MAX_LENGTH &&
    GROUP_ID_PATTERN.test(normalized)
  );
};

/**
 * Normalize strict groups without changing their order. A group must have a
 * unique id and at least one unique, addressable provider. Passing an
 * addressable list makes stale providers fail closed instead of silently
 * disappearing from a user's manifest.
 */
export const normalizeGatewayAggregateGroups = (
  groups: readonly GatewayAggregateGroup[] | null | undefined,
  addressableSiteIds?: readonly string[],
): GatewayAggregateGroup[] | null => {
  if (!groups || groups.length === 0) {
    return [];
  }
  const addressable = addressableSiteIds ? new Set(addressableSiteIds) : null;
  const seenGroupIds = new Set<string>();
  const normalized: GatewayAggregateGroup[] = [];

  for (const group of groups) {
    if (!group || typeof group.id !== 'string' || !Array.isArray(group.provider_ids)) {
      return null;
    }
    const id = group.id.trim();
    if (!validateGatewayAggregateGroupId(id) || seenGroupIds.has(id.toLowerCase())) {
      return null;
    }
    // Do not silently deduplicate a group. The group order is the routing
    // priority, so a repeated provider is an invalid draft even though the
    // flattened compatibility list is globally deduplicated later.
    const providerIds: string[] = [];
    const seenProviderIds = new Set<string>();
    for (const rawProviderId of group.provider_ids) {
      if (typeof rawProviderId !== 'string') {
        return null;
      }
      const providerId = rawProviderId.trim();
      if (!isAggregateSiteId(providerId) || seenProviderIds.has(providerId)) {
        return null;
      }
      seenProviderIds.add(providerId);
      providerIds.push(providerId);
    }
    if (providerIds.length === 0) {
      return null;
    }
    if (addressable && providerIds.some((providerId) => !addressable.has(providerId))) {
      return null;
    }
    seenGroupIds.add(id.toLowerCase());
    normalized.push({ id, provider_ids: providerIds });
  }
  return normalized;
};

/** Flatten providers in group order for the aggregate command payload. */
export const flattenGatewayAggregateGroups = (
  groups: readonly GatewayAggregateGroup[] | null | undefined,
): string[] => {
  const flattened: string[] = [];
  const seen = new Set<string>();
  for (const group of groups ?? []) {
    for (const providerId of group.provider_ids) {
      const normalized = providerId.trim();
      if (!normalized || seen.has(normalized)) continue;
      seen.add(normalized);
      flattened.push(normalized);
    }
  }
  return flattened;
};

/**
 * Serialize aggregate submissions while allowing callers to discard stale
 * payloads before they touch the backend or update UI state.
 *
 * A request that is already in flight cannot be cancelled, so the queue keeps
 * later requests behind it. The `isCurrent` callback lets the operation skip
 * its follow-up status refresh when a newer payload superseded it.
 */
export const createLatestGatewayAggregateOperationQueue = () => {
  let latestRevision = 0;
  let tail: Promise<void> = Promise.resolve();

  return {
    enqueue<T>(operation: (isCurrent: () => boolean) => Promise<T>): Promise<T | undefined> {
      const revision = ++latestRevision;
      const run = tail.then(async () => {
        if (revision !== latestRevision) {
          return undefined;
        }
        return operation(() => revision === latestRevision);
      });
      tail = run.then(
        () => undefined,
        () => undefined,
      );
      return run;
    },
  };
};

export const normalizeGatewayAggregateAliases = (
  aliases: Record<string, string> | null | undefined,
  selectedSiteIds: readonly string[],
  addressableSiteIds: readonly string[] = selectedSiteIds,
): Record<string, string> | null => {
  const selected = new Set(selectedSiteIds);
  const addressable = new Set(addressableSiteIds);
  if (selectedSiteIds.some((siteId) => !addressable.has(siteId))) {
    return null;
  }
  const normalized: Record<string, string> = {};
  const seenAliases = new Set<string>();
  for (const [siteId, rawAlias] of Object.entries(aliases ?? {})) {
    // The DTO is typed as string, but saved JSON can still contain malformed
    // values. Fail closed instead of throwing while reopening the settings.
    if (typeof rawAlias !== 'string') return null;
    const alias = rawAlias.trim();
    if (!alias) continue;
    if (!selected.has(siteId) || !validateGatewayAggregateAlias(alias)) return null;
    const key = alias.toLowerCase();
    if (seenAliases.has(key)) return null;
    seenAliases.add(key);
    normalized[siteId] = alias;
  }
  const seenPrefixes = new Set<string>();
  for (const siteId of addressableSiteIds) {
    const effective = normalized[siteId] ?? siteId;
    const key = effective.toLowerCase();
    if (seenPrefixes.has(key)) return null;
    seenPrefixes.add(key);
  }
  return normalized;
};

/**
 * Remove aliases that no longer have a selected/addressable provider while
 * loading an existing manifest. Keep invalid aliases for selected providers so
 * normal validation can still fail closed instead of silently repairing a
 * persisted routing rule.
 */
export const pruneStaleGatewayAggregateAliases = (
  aliases: Record<string, string> | null | undefined,
  selectedSiteIds: readonly string[],
  addressableSiteIds?: readonly string[],
): Record<string, string> => {
  const selected = new Set(selectedSiteIds);
  // While provider candidates are still loading, the caller does not yet
  // know which saved sites are addressable. Preserve selected aliases and
  // let the loaded-candidates pass below prune only genuinely stale sites.
  if (addressableSiteIds === undefined) {
    const preserved: Record<string, string> = {};
    for (const [siteId, rawAlias] of Object.entries(aliases ?? {})) {
      if (selected.has(siteId) && typeof rawAlias === 'string') {
        preserved[siteId] = rawAlias;
      }
    }
    return preserved;
  }
  const addressable = new Set(addressableSiteIds);
  const pruned: Record<string, string> = {};
  for (const [siteId, rawAlias] of Object.entries(aliases ?? {})) {
    if (!selected.has(siteId) || !addressable.has(siteId) || typeof rawAlias !== 'string') {
      continue;
    }
    pruned[siteId] = rawAlias;
  }
  return pruned;
};

/** Build the canonical aggregate payload for an alias edit, when it can be
 * applied immediately to an already engaged takeover. */
export const prepareGatewayAggregateAliasReengage = (
  currentAliases: Record<string, string>,
  siteId: string,
  alias: string,
  siteIds: readonly string[],
  engaged: boolean,
  separator: string,
  naming: GatewayAggregateNamingMode,
  addressableSiteIds: readonly string[] = siteIds,
  groups: readonly GatewayAggregateGroup[] = [],
): {
  siteIds: string[];
  separator: string;
  aliases: Record<string, string>;
  naming: GatewayAggregateNamingMode;
  groups?: GatewayAggregateGroup[];
} | null => {
  const nextAliases = { ...currentAliases, [siteId]: alias };
  if (!alias.trim()) delete nextAliases[siteId];
  const normalizedAliases = normalizeGatewayAggregateAliases(nextAliases, siteIds, addressableSiteIds);
  if (!engaged || siteIds.length === 0 || !normalizedAliases) {
    return null;
  }
  return {
    siteIds: [...siteIds],
    separator: normalizeGatewayAggregateSeparator(separator),
    aliases: normalizedAliases,
    naming,
    ...(groups.length > 0 ? { groups: groups.map((group) => ({ ...group, provider_ids: [...group.provider_ids] })) } : {}),
  };
};

/** Drop duplicate/non-addressable site ids while preserving the user's order. */
export const normalizeGatewayAggregateSiteIds = (siteIds: readonly string[]): string[] => {
  const seen = new Set<string>();
  const normalized: string[] = [];
  for (const siteId of siteIds) {
    const trimmed = siteId.trim();
    if (!isAggregateSiteId(trimmed) || seen.has(trimmed)) {
      continue;
    }
    seen.add(trimmed);
    normalized.push(trimmed);
  }
  return normalized;
};

/**
 * Read the aggregate selection that must be replayed when re-engaging.
 *
 * Returns `null` when the backend did not expose aggregate details, so callers
 * can skip the aggregate round trip instead of silently re-engaging with an
 * empty site list (which would drop the whole cross-site model list).
 */
export const toGatewayAggregateReengageConfig = (
  status?: GatewayCliTakeoverStatus | null,
  addressableSiteIds?: readonly string[],
): GatewayAggregateReengageConfig | null => {
  if (!status || !isGatewayAggregateMode(status.mode)) {
    return null;
  }
  const aggregate = status.aggregate ?? null;
  if (!aggregate) {
    return null;
  }
  const providerIds = normalizeGatewayAggregateSiteIds(aggregate.provider_ids ?? []);
  const addressableIds =
    addressableSiteIds ??
    (status.provider_priorities.length > 0
      ? status.provider_priorities.map((entry) => entry.provider_id)
      : undefined);
  const groups = normalizeGatewayAggregateGroups(
    aggregate.groups,
    addressableIds,
  );
  if (!groups) return null;
  // A non-empty groups list is the strict form. It is self-contained: the
  // group order and membership are authoritative, while legacy separator,
  // aliases, naming, and the flat compatibility list are ignored.
  if (groups.length > 0) {
    const canonicalProviderIds = flattenGatewayAggregateGroups(groups);
    if (canonicalProviderIds.length === 0) return null;
    return {
      providerIds: canonicalProviderIds,
      separator: DEFAULT_AGGREGATE_SEPARATOR,
      aliases: {},
      naming: 'site_model',
      groups,
    };
  }

  const separator = normalizeGatewayAggregateSeparator(aggregate.separator ?? '');
  if (providerIds.length === 0 || validateGatewayAggregateSeparator(separator) !== null) {
    return null;
  }
  if (addressableIds && providerIds.some((providerId) => !addressableIds.includes(providerId))) {
    return null;
  }
  const aliases = normalizeGatewayAggregateAliases(
    aggregate.aliases,
    providerIds,
    addressableIds ?? providerIds,
  );
  const naming: GatewayAggregateNamingMode = aggregate.naming ?? 'site_model';
  if (!aliases || !['site_model', 'model_at_site', 'model_only'].includes(naming)) {
    return null;
  }
  return { providerIds, separator, aliases, naming, groups: [] };
};

/**
 * Decide which takeover mode a provider save must replay around itself.
 *
 * `single` and `failover` behave exactly as before. `aggregate` is only
 * replayable when its selection is available, so a missing/incomplete status
 * degrades to "no re-engage" instead of engaging a mode with no sites.
 */
export const resolveGatewayReengageMode = (
  status?: GatewayCliTakeoverStatus | null,
  addressableSiteIds?: readonly string[],
): 'single' | 'failover' | 'aggregate' | null => {
  const mode = status?.mode ?? null;
  if (!isGatewayReengageMode(mode)) {
    return null;
  }
  if (isGatewayAggregateMode(mode) && !toGatewayAggregateReengageConfig(status, addressableSiteIds)) {
    return null;
  }
  return mode;
};

/** Label of the model list entry Codex sees for one (site, model) pair. */
export const buildGatewayAggregateModelSlug = (
  siteId: string,
  modelId: string,
  separator: string,
  naming: GatewayAggregateNamingMode = 'site_model',
): string => {
  if (naming === 'model_only') return modelId;
  return naming === 'model_at_site'
    ? `${modelId}${separator}${siteId}`
    : `${siteId}${separator}${modelId}`;
};

/** Strict-group slugs always use the canonical `group.model` shape. */
export const buildGatewayAggregateGroupModelSlug = (
  groupId: string,
  modelId: string,
): string => `${groupId.trim()}.${modelId}`;
