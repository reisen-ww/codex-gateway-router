import React from 'react';
import { Switch } from 'antd';
import { ArrowDown, ArrowUp, GripVertical, Loader2, Plus, Route, Trash2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import {
  DndContext,
  KeyboardSensor,
  PointerSensor,
  closestCenter,
  useSensor,
  useSensors,
  type DragEndEvent,
} from '@dnd-kit/core';
import {
  SortableContext,
  arrayMove,
  sortableKeyboardCoordinates,
  useSortable,
  verticalListSortingStrategy,
} from '@dnd-kit/sortable';
import { restrictToVerticalAxis } from '@dnd-kit/modifiers';
import { CSS } from '@dnd-kit/utilities';
import {
  DEFAULT_AGGREGATE_SEPARATOR,
  engageProxyGatewayAggregate,
  getProxyGatewayCliStatuses,
  restoreProxyGatewayCliDirect,
  type GatewayCliKey,
  type GatewayAggregateGroup,
  type GatewayAggregateNamingMode,
  type GatewayCliTakeoverStatus,
} from '@/services';
import { listCodexProviders } from '@/services/codexApi';
import {
  buildGatewayAggregateGroupModelSlug,
  createLatestGatewayAggregateOperationQueue,
  flattenGatewayAggregateGroups,
  normalizeGatewayAggregateSeparator,
  normalizeGatewayAggregateGroups,
  pruneStaleGatewayAggregateAliases,
  validateGatewayAggregateGroupId,
} from '@/features/coding/shared/gateway/gatewayAggregateConfig';
import {
  buildGatewayAggregateModelSlug,
  isAggregateSiteId,
  isGatewayAggregateMode,
  moveAggregateSite,
  normalizeGatewayAggregateAliases,
  normalizeGatewayAggregateSiteIds,
  toAggregateSiteCandidates,
  validateGatewayAggregateSeparator,
  validateGatewayAggregateAlias,
  type GatewayAggregateSiteCandidate,
} from '@/features/coding/shared/gateway';
import styles from './GatewayAggregateSettings.module.less';

// The backend aggregate manifest and model catalog are Codex-specific for now.
// Keep the selector honest instead of exposing CLI choices that the command
// layer will reject.
type AggregateCliKey = Extract<GatewayCliKey, 'codex'>;

const AGGREGATE_CLI_KEYS: AggregateCliKey[] = [
  'codex',
];

/**
 * Reuse the providers page data source per CLI; the gateway cannot reach any
 * provider the CLI page does not already own, so no extra request is invented.
 */
const loadProviders = async (
  cliKey: AggregateCliKey,
): Promise<GatewayAggregateSiteCandidate[]> => {
  switch (cliKey) {
    case 'codex':
      return toAggregateSiteCandidates(await listCodexProviders());
    default:
      return [];
  }
};

const formatError = (error: unknown) =>
  error instanceof Error ? error.message : String(error);

interface SortableSiteRowProps {
  candidate: GatewayAggregateSiteCandidate;
  index: number;
  lastIndex: number;
  onToggleSite: (siteId: string, checked: boolean) => void;
  onMoveSite: (siteId: string, direction: 'up' | 'down') => void;
  alias: string;
  onAliasChange: (siteId: string, alias: string) => void;
  onAliasCommit: () => void;
  disabled: boolean;
  aliasEditable?: boolean;
  groupId?: string;
}

/**
 * Selected site row. Order is the aggregate fallback priority, so it is
 * reorderable by drag handle and by keyboard-accessible up/down buttons
 * (DESIGN.md requires an equivalent non-drag path).
 */
const SortableSiteRow: React.FC<SortableSiteRowProps> = ({
  candidate,
  index,
  lastIndex,
  onToggleSite,
  onMoveSite,
  alias,
  onAliasChange,
  onAliasCommit,
  disabled,
  aliasEditable = true,
  groupId,
}) => {
  const { t } = useTranslation();
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: candidate.id,
    disabled,
  });
  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
    opacity: isDragging ? 0.6 : undefined,
  };

  return (
    <li ref={setNodeRef} style={style} className={styles.siteItem}>
      <span
        className={styles.dragHandle}
        title={t('gateway.aggregate.reorderHint')}
        aria-label={t('gateway.aggregate.reorderHint')}
        {...attributes}
        {...listeners}
      >
        <GripVertical size={13} aria-hidden="true" />
      </span>
      <input
        type="checkbox"
        checked
        disabled={disabled}
        aria-label={candidate.name}
        onChange={(event) => onToggleSite(candidate.id, event.currentTarget.checked)}
      />
      <span className={styles.siteName} title={candidate.name}>
        {candidate.name}
      </span>
      <code className={styles.siteSlug} title={candidate.id}>
        {candidate.id}
      </code>
      {aliasEditable ? (
        <input
          className={styles.aliasInput}
          value={alias}
          disabled={disabled}
          maxLength={32}
          placeholder={t('gateway.aggregate.aliasPlaceholder')}
          aria-label={`${candidate.name}: ${t('gateway.aggregate.alias')}`}
          aria-invalid={alias.length > 0 && !validateGatewayAggregateAlias(alias)}
          onChange={(event) => onAliasChange(candidate.id, event.currentTarget.value)}
          onBlur={onAliasCommit}
        />
      ) : (
        <code className={styles.groupModelSlug} title={groupId ? `${groupId}.model` : 'group.model'}>
          {groupId ? `${groupId}.model` : 'group.model'}
        </code>
      )}
      <span className={styles.siteActions}>
        <button
          type="button"
          className={styles.iconButton}
          disabled={disabled || index === 0}
          aria-label={`${candidate.name}: ${t('gateway.aggregate.moveUp')}`}
          onClick={() => onMoveSite(candidate.id, 'up')}
        >
          <ArrowUp size={13} aria-hidden="true" />
        </button>
        <button
          type="button"
          className={styles.iconButton}
          disabled={disabled || index === lastIndex}
          aria-label={`${candidate.name}: ${t('gateway.aggregate.moveDown')}`}
          onClick={() => onMoveSite(candidate.id, 'down')}
        >
          <ArrowDown size={13} aria-hidden="true" />
        </button>
      </span>
    </li>
  );
};

interface GatewayAggregateSettingsProps {
  /** Gateway must be running before any engage command can succeed. */
  running: boolean;
  /**
   * Notified after a successful engage/disengage so the settings panel can
   * refresh its own takeover list. Must be stable (useCallback with no deps) —
   * it is not called on load, so it cannot feed back into the seed effect.
   */
  onTakeoverChange?: () => void;
}

type GatewayAggregateGroupDraft = GatewayAggregateGroup & {
  /** Stable UI-only identity: a group id can be edited before it is committed. */
  draftKey: string;
};

const GatewayAggregateSettings: React.FC<GatewayAggregateSettingsProps> = ({
  running,
  onTakeoverChange,
}) => {
  const { t } = useTranslation();
  const [cliKey, setCliKey] = React.useState<AggregateCliKey>('codex');
  const [candidates, setCandidates] = React.useState<GatewayAggregateSiteCandidate[]>([]);
  const [loadingSites, setLoadingSites] = React.useState(true);
  const [siteIds, setSiteIds] = React.useState<string[]>([]);
  const [separator, setSeparator] = React.useState<string>(DEFAULT_AGGREGATE_SEPARATOR);
  const [aliases, setAliases] = React.useState<Record<string, string>>({});
  const [naming, setNaming] = React.useState<GatewayAggregateNamingMode>('site_model');
  const [groups, setGroups] = React.useState<GatewayAggregateGroupDraft[]>([]);
  const [cliStatuses, setCliStatuses] = React.useState<GatewayCliTakeoverStatus[]>([]);
  const [busy, setBusy] = React.useState(false);
  const [notice, setNotice] = React.useState<{ kind: 'error' | 'success'; text: string } | null>(
    null,
  );
  const revisionRef = React.useRef(0);
  const statusRequestRef = React.useRef(0);
  const mountedRef = React.useRef(true);
  const operationQueueRef = React.useRef(createLatestGatewayAggregateOperationQueue());
  const groupDraftKeyRef = React.useRef(0);
  const createGroupDraft = React.useCallback(
    (group: GatewayAggregateGroup): GatewayAggregateGroupDraft => ({
      ...group,
      draftKey: `gateway-aggregate-group-${++groupDraftKeyRef.current}`,
    }),
    [],
  );

  const selectedStatus = React.useMemo(
    () => cliStatuses.find((status) => status.cli_key === cliKey) ?? null,
    [cliKey, cliStatuses],
  );
  const engaged = isGatewayAggregateMode(selectedStatus?.mode);
  const strictGroups = groups.length > 0;
  const separatorError = validateGatewayAggregateSeparator(separator);
  const effectiveSeparator = normalizeGatewayAggregateSeparator(separator);
  const addressableSiteIds = React.useMemo(
    () => candidates.map((candidate) => candidate.id),
    [candidates],
  );
  const normalizedGroups = normalizeGatewayAggregateGroups(groups, addressableSiteIds);
  const groupedSiteIds = React.useMemo(
    () => flattenGatewayAggregateGroups(groups),
    [groups],
  );
  const normalizedAliases = normalizeGatewayAggregateAliases(
    aliases,
    siteIds,
    addressableSiteIds,
  );
  const normalizedSiteIds = strictGroups
    ? flattenGatewayAggregateGroups(normalizedGroups ?? [])
    : normalizeGatewayAggregateSiteIds(siteIds);
  const canEngage =
    running &&
    normalizedSiteIds.length > 0 &&
    (strictGroups
      ? normalizedGroups !== null
      : separatorError === null && normalizedAliases !== null) &&
    !busy;
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 8 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  const applyCliStatuses = React.useCallback(
    (statuses: GatewayCliTakeoverStatus[]) => {
      setCliStatuses(statuses);
    },
    [],
  );

  const refreshCliStatuses = React.useCallback(async () => {
    const request = statusRequestRef.current + 1;
    statusRequestRef.current = request;
    const statuses = await getProxyGatewayCliStatuses();
    if (mountedRef.current && statusRequestRef.current === request) {
      applyCliStatuses(statuses);
    }
    return statuses;
  }, [applyCliStatuses]);

  React.useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      revisionRef.current += 1;
    };
  }, []);

  // Seed the takeover state from the backend manifest so reopening the settings
  // page shows what is actually routing, not an empty form.
  React.useEffect(() => {
    let disposed = false;
    const statusRequest = statusRequestRef.current + 1;
    statusRequestRef.current = statusRequest;
    const load = async () => {
      try {
        const statuses = await getProxyGatewayCliStatuses();
        if (!disposed && mountedRef.current && statusRequestRef.current === statusRequest) {
          applyCliStatuses(statuses);
        }
      } catch {
        if (!disposed && mountedRef.current && statusRequestRef.current === statusRequest) {
          applyCliStatuses([]);
        }
      }
    };
    void load();
    return () => {
      disposed = true;
    };
  }, [applyCliStatuses]);

  React.useEffect(() => {
    let disposed = false;
    const request = revisionRef.current + 1;
    revisionRef.current = request;
    setLoadingSites(true);

    const load = async () => {
      try {
        const next = await loadProviders(cliKey);
        if (disposed || revisionRef.current !== request) {
          return;
        }
        setCandidates(next);
      } catch (error) {
        if (!disposed && revisionRef.current === request) {
          setCandidates([]);
          setNotice({ kind: 'error', text: formatError(error) });
        }
      } finally {
        if (!disposed && revisionRef.current === request) {
          setLoadingSites(false);
        }
      }
    };

    void load();
    return () => {
      disposed = true;
    };
  }, [cliKey]);

  // Load the saved aggregate config for the selected CLI, and drop sites that
  // are no longer proxyable instead of showing them as still selected. Keyed on
  // the backend status so a re-engage round trip re-seeds the canonical list.
  React.useEffect(() => {
    const saved = selectedStatus && isGatewayAggregateMode(selectedStatus.mode)
      ? selectedStatus.aggregate ?? null
      : null;
    const savedSeparator = saved?.separator
      ? normalizeGatewayAggregateSeparator(saved.separator)
      : DEFAULT_AGGREGATE_SEPARATOR;
    setSeparator(
      validateGatewayAggregateSeparator(savedSeparator) === null
        ? savedSeparator
        : DEFAULT_AGGREGATE_SEPARATOR,
    );
    setNaming(saved?.naming ?? 'site_model');
    const savedGroups = (saved?.groups ?? []).map((group) => ({
      id: group.id,
      provider_ids: [...group.provider_ids],
    }));
    const savedSiteIds = savedGroups.length > 0
      ? flattenGatewayAggregateGroups(savedGroups)
      : normalizeGatewayAggregateSiteIds(saved?.provider_ids ?? []);
    setAliases(
      savedGroups.length > 0
        ? {}
        : pruneStaleGatewayAggregateAliases(
            saved?.aliases,
            savedSiteIds,
            loadingSites ? undefined : addressableSiteIds,
          ),
    );
    setGroups(savedGroups.map(createGroupDraft));
    if (!saved) {
      setSiteIds([]);
      return;
    }
    // Keep stale ids in the draft so an unavailable provider/alias is visible
    // and recoverable instead of silently deleting user configuration. In
    // strict mode, the grouped order is the canonical compatibility order.
    setSiteIds(savedSiteIds);
  }, [addressableSiteIds, candidates, createGroupDraft, loadingSites, selectedStatus]);

  const runGatewayOperation = React.useCallback(
    async (
      execute: () => Promise<unknown>,
      successText: string,
      failureKey: 'enableFailed' | 'disableFailed',
    ) => {
      setBusy(true);
      setNotice(null);
      await operationQueueRef.current.enqueue(async (isCurrent) => {
        const isMountedAndCurrent = () => mountedRef.current && isCurrent();
        try {
          await execute();
          if (!isMountedAndCurrent()) {
            return;
          }
          await refreshCliStatuses();
          if (!isMountedAndCurrent()) {
            return;
          }
          onTakeoverChange?.();
          setNotice({ kind: 'success', text: successText });
        } catch (error) {
          if (isMountedAndCurrent()) {
            setNotice({
              kind: 'error',
              text: t(`gateway.aggregate.notice.${failureKey}`, { error: formatError(error) }),
            });
          }
        } finally {
          if (isMountedAndCurrent()) {
            setBusy(false);
          }
        }
      });
    },
    [onTakeoverChange, refreshCliStatuses, t],
  );

  const runEngage = React.useCallback(
    (
      nextSiteIds: string[],
      nextSeparator: string,
      nextAliases: Record<string, string>,
      nextNaming: GatewayAggregateNamingMode,
      nextGroups: GatewayAggregateGroup[] = [],
    ) => {
      // Draft keys only exist to keep editable rows mounted. Do not pass them
      // across the frontend/backend boundary.
      const requestGroups = nextGroups.map((group) => ({
        id: group.id,
        provider_ids: [...group.provider_ids],
      }));
      return runGatewayOperation(
        () =>
          engageProxyGatewayAggregate(
            cliKey,
            nextSiteIds,
            nextSeparator,
            nextAliases,
            nextNaming,
            requestGroups,
          ),
        t('gateway.aggregate.notice.enabled'),
        'enableFailed',
      );
    },
    [cliKey, runGatewayOperation, t],
  );

  const runRestore = React.useCallback(
    () =>
      runGatewayOperation(
        () => restoreProxyGatewayCliDirect(cliKey),
        t('gateway.aggregate.notice.disabled'),
        'disableFailed',
      ),
    [cliKey, runGatewayOperation, t],
  );

  const handleToggle = async (checked: boolean) => {
    setNotice(null);
    if (!checked) {
      await runRestore();
      return;
    }

    if (strictGroups) {
      if (!normalizedGroups) {
        setNotice({ kind: 'error', text: t('gateway.aggregate.groupsInvalid') });
        return;
      }
      const nextSiteIds = flattenGatewayAggregateGroups(normalizedGroups);
      if (nextSiteIds.length === 0) {
        setNotice({ kind: 'error', text: t('gateway.aggregate.sitesRequired') });
        return;
      }
      await runEngage(
        nextSiteIds,
        DEFAULT_AGGREGATE_SEPARATOR,
        {},
        'site_model',
        normalizedGroups,
      );
      return;
    }
    const nextSiteIds = normalizeGatewayAggregateSiteIds(siteIds);
    if (nextSiteIds.length === 0) {
      setNotice({ kind: 'error', text: t('gateway.aggregate.sitesRequired') });
      return;
    }
    if (validateGatewayAggregateSeparator(effectiveSeparator) !== null) {
      setNotice({ kind: 'error', text: t('gateway.aggregate.notice.invalidConfig') });
      return;
    }
    if (!normalizedAliases) {
      setNotice({ kind: 'error', text: t('gateway.aggregate.aliasInvalid') });
      return;
    }
    await runEngage(
      nextSiteIds,
      effectiveSeparator,
      normalizedAliases,
      naming,
      [],
    );
  };

  const handleToggleSite = async (siteId: string, checked: boolean) => {
    const nextSiteIds = checked
      ? normalizeGatewayAggregateSiteIds([...siteIds, siteId])
      : siteIds.filter((item) => item !== siteId);
    setSiteIds(nextSiteIds);
    if (!checked && siteId in aliases) {
      const nextAliases = { ...aliases };
      delete nextAliases[siteId];
      setAliases(nextAliases);
    }
    // Auto-save: a running aggregate takeover must follow the new site list.
    if (!engaged) {
      return;
    }
    if (nextSiteIds.length === 0) {
      // Aggregate mode cannot represent an empty site list. Restoring direct
      // mode is safer than leaving the backend on the stale selection.
      await handleToggle(false);
      return;
    }
    const nextAliases = normalizeGatewayAggregateAliases(
      aliases,
      nextSiteIds,
      addressableSiteIds,
    );
    if (separatorError === null && nextAliases) {
      void runEngage(nextSiteIds, effectiveSeparator, nextAliases, naming, []);
    }
  };

  const handleMoveSite = (siteId: string, direction: 'up' | 'down') => {
    const nextSiteIds = moveAggregateSite(siteIds, siteId, direction);
    setSiteIds(nextSiteIds);
    if (
      engaged &&
      nextSiteIds.length > 0 &&
      separatorError === null &&
      normalizedAliases &&
      !strictGroups
    ) {
      void runEngage(nextSiteIds, effectiveSeparator, normalizedAliases, naming, []);
    }
  };

  const handleDragEnd = (event: DragEndEvent) => {
    const { active, over } = event;
    if (!over || active.id === over.id) {
      return;
    }
    const oldIndex = siteIds.indexOf(String(active.id));
    const newIndex = siteIds.indexOf(String(over.id));
    if (oldIndex < 0 || newIndex < 0) {
      return;
    }
    const nextSiteIds = arrayMove(siteIds, oldIndex, newIndex);
    setSiteIds(nextSiteIds);
    if (
      engaged &&
      nextSiteIds.length > 0 &&
      separatorError === null &&
      normalizedAliases &&
      !strictGroups
    ) {
      void runEngage(nextSiteIds, effectiveSeparator, normalizedAliases, naming, []);
    }
  };

  const handleSeparatorCommit = () => {
    if (validateGatewayAggregateSeparator(effectiveSeparator) !== null) {
      return;
    }
    setSeparator(effectiveSeparator);
    if (engaged && !strictGroups && siteIds.length > 0 && normalizedAliases) {
      void runEngage(siteIds, effectiveSeparator, normalizedAliases, naming, []);
    }
  };

  const handleAliasCommit = () => {
    if (!engaged || strictGroups || siteIds.length === 0) {
      return;
    }
    const nextAliases = normalizeGatewayAggregateAliases(
      aliases,
      siteIds,
      addressableSiteIds,
    );
    if (!nextAliases) {
      setNotice({ kind: 'error', text: t('gateway.aggregate.aliasInvalid') });
      return;
    }
    if (separatorError === null) {
      void runEngage(siteIds, effectiveSeparator, nextAliases, naming, []);
    }
  };

  const reengageWithGroups = React.useCallback(
    (
      nextGroups: GatewayAggregateGroup[],
      nextSiteIds: string[] = siteIds,
      nextAliases: Record<string, string> = aliases,
      nextNaming: GatewayAggregateNamingMode = naming,
    ) => {
      if (!engaged) return;
      const normalizedGroupValues = normalizeGatewayAggregateGroups(
        nextGroups,
        addressableSiteIds,
      );
      if (nextGroups.length > 0) {
        const normalizedSiteIds = flattenGatewayAggregateGroups(normalizedGroupValues ?? []);
        if (normalizedSiteIds.length === 0 || !normalizedGroupValues) return;
        void runEngage(
          normalizedSiteIds,
          DEFAULT_AGGREGATE_SEPARATOR,
          {},
          'site_model',
          normalizedGroupValues,
        );
        return;
      }

      const normalizedSiteIds = normalizeGatewayAggregateSiteIds(nextSiteIds);
      const normalizedAliasValues = normalizeGatewayAggregateAliases(
        nextAliases,
        normalizedSiteIds,
        addressableSiteIds,
      );
      if (
        normalizedSiteIds.length === 0 ||
        separatorError !== null ||
        !normalizedAliasValues
      ) {
        return;
      }
      void runEngage(
        normalizedSiteIds,
        effectiveSeparator,
        normalizedAliasValues,
        nextNaming,
        [],
      );
    },
    [
      addressableSiteIds,
      aliases,
      effectiveSeparator,
      engaged,
      naming,
      runEngage,
      separatorError,
      siteIds,
    ],
  );

  const handleAddGroup = () => {
    const used = new Set(groups.map((group) => group.id.toLowerCase()));
    let index = groups.length + 1;
    while (used.has(`group-${index}`)) index += 1;
    // Converting an existing legacy selection should not make the selected
    // sites disappear from the editor. New groups added after that start
    // empty and can intentionally reuse providers from another group.
    const initialProviderIds = groups.length === 0
      ? normalizeGatewayAggregateSiteIds(siteIds)
      : [];
    const nextGroups = [
      ...groups,
      createGroupDraft({ id: `group-${index}`, provider_ids: initialProviderIds }),
    ];
    setGroups(nextGroups);
    const nextSiteIds = flattenGatewayAggregateGroups(nextGroups);
    setSiteIds(nextSiteIds);
    setNotice(null);
    if (engaged) {
      reengageWithGroups(nextGroups, nextSiteIds);
    }
  };

  const handleUseLegacyAggregate = () => {
    const nextSiteIds = groupedSiteIds.length > 0
      ? groupedSiteIds
      : normalizeGatewayAggregateSiteIds(siteIds);
    setGroups([]);
    setSiteIds(nextSiteIds);
    setNotice(null);
    if (!engaged) return;
    const nextAliases = normalizeGatewayAggregateAliases(
      aliases,
      nextSiteIds,
      addressableSiteIds,
    );
    if (
      nextSiteIds.length > 0 &&
      separatorError === null &&
      nextAliases
    ) {
      void runEngage(nextSiteIds, effectiveSeparator, nextAliases, naming, []);
    }
  };

  const handleRemoveGroup = (groupDraftKey: string) => {
    const nextGroups = groups.filter((group) => group.draftKey !== groupDraftKey);
    setGroups(nextGroups);
    setSiteIds(nextGroups.length > 0
      ? flattenGatewayAggregateGroups(nextGroups)
      : groupedSiteIds);
    reengageWithGroups(nextGroups);
  };

  const handleGroupIdChange = (groupDraftKey: string, id: string) => {
    setGroups(
      groups.map((group) => (group.draftKey === groupDraftKey ? { ...group, id } : group)),
    );
  };

  const handleGroupIdCommit = () => {
    const normalized = normalizeGatewayAggregateGroups(groups, addressableSiteIds);
    if (!normalized) {
      setNotice({ kind: 'error', text: t('gateway.aggregate.groupsInvalid') });
      return;
    }
    const nextGroups = normalized.map((group, index) => {
      const currentDraft = groups[index];
      return currentDraft
        ? { ...group, draftKey: currentDraft.draftKey }
        : createGroupDraft(group);
    });
    setGroups(nextGroups);
    reengageWithGroups(nextGroups);
  };

  const handleToggleGroupSite = (
    groupDraftKey: string,
    siteId: string,
    checked: boolean,
  ) => {
    const nextGroups = groups.map((group) => {
      if (group.draftKey !== groupDraftKey) return group;
      const provider_ids = checked
        ? normalizeGatewayAggregateSiteIds([...group.provider_ids, siteId])
        : group.provider_ids.filter((item) => item !== siteId);
      return { ...group, provider_ids };
    });
    const nextSiteIds = flattenGatewayAggregateGroups(nextGroups);
    setGroups(nextGroups);
    setSiteIds(nextSiteIds);
    reengageWithGroups(nextGroups, nextSiteIds);
  };

  const handleMoveGroupSite = (
    groupDraftKey: string,
    siteId: string,
    direction: 'up' | 'down',
  ) => {
    const nextGroups = groups.map((group) =>
      group.draftKey === groupDraftKey
        ? { ...group, provider_ids: moveAggregateSite(group.provider_ids, siteId, direction) }
        : group,
    );
    setGroups(nextGroups);
    setSiteIds(flattenGatewayAggregateGroups(nextGroups));
    reengageWithGroups(nextGroups);
  };

  const handleGroupDragEnd = (groupDraftKey: string, event: DragEndEvent) => {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const group = groups.find((item) => item.draftKey === groupDraftKey);
    if (!group) return;
    const oldIndex = group.provider_ids.indexOf(String(active.id));
    const newIndex = group.provider_ids.indexOf(String(over.id));
    if (oldIndex < 0 || newIndex < 0) return;
    const nextGroups = groups.map((item) =>
      item.draftKey === groupDraftKey
        ? { ...item, provider_ids: arrayMove(item.provider_ids, oldIndex, newIndex) }
        : item,
    );
    setGroups(nextGroups);
    setSiteIds(flattenGatewayAggregateGroups(nextGroups));
    reengageWithGroups(nextGroups);
  };

  const handleClearSelection = () => {
    if (engaged) {
      // Clearing the last aggregate site is equivalent to leaving aggregate
      // mode; keep the backend and the visible form in sync.
      void handleToggle(false);
      return;
    }
    setSiteIds([]);
  };

  const selectedCandidates = siteIds
    .map((siteId) => candidates.find((candidate) => candidate.id === siteId))
    .filter((candidate): candidate is GatewayAggregateSiteCandidate => Boolean(candidate));
  const unselectedCandidates = candidates.filter((candidate) => !siteIds.includes(candidate.id));
  const separatorExample = strictGroups
    ? buildGatewayAggregateGroupModelSlug(groups[0]?.id || 'group', 'model')
    : buildGatewayAggregateModelSlug(
        aliases[candidates[0]?.id ?? ''] || candidates[0]?.id || 'site-id',
        'model',
        separatorError === null ? effectiveSeparator : DEFAULT_AGGREGATE_SEPARATOR,
        naming,
      );
  const displayedNaming: GatewayAggregateNamingMode = strictGroups ? 'site_model' : naming;
  const displayedSeparator = strictGroups ? DEFAULT_AGGREGATE_SEPARATOR : separator;
  const namingHint = strictGroups
    ? t('gateway.aggregate.strictNamingHint')
    : t('gateway.aggregate.namingHint');
  const separatorHint = strictGroups
    ? t('gateway.aggregate.strictSeparatorHint')
    : t('gateway.aggregate.separatorHint', { example: separatorExample });
  const invalidSiteIds = siteIds.filter((siteId) => !isAggregateSiteId(siteId));
  const staleSiteIds = siteIds.filter((siteId) => !addressableSiteIds.includes(siteId));
  const staleGroupSiteIds = groups.flatMap((group) =>
    group.provider_ids.filter((siteId) => !addressableSiteIds.includes(siteId)),
  );

  return (
    <div className={styles.settings}>
      <div className={styles.toolbar}>
        <label className={styles.cliPicker}>
          <span>{t('gateway.aggregate.cliLabel')}</span>
          <select
            className={styles.select}
            value={cliKey}
            disabled={busy}
            onChange={(event) => setCliKey(event.currentTarget.value as AggregateCliKey)}
          >
            {AGGREGATE_CLI_KEYS.map((option) => (
              <option key={option} value={option}>
                {t(`settings.gateway.cli.${option}`)}
              </option>
            ))}
          </select>
        </label>
        <div className={styles.toggle}>
          <span className={styles.state} role="status">
            {engaged ? t('gateway.aggregate.enabledLabel') : t('gateway.aggregate.disabledLabel')}
          </span>
          <Switch
            size="small"
            checked={engaged}
            disabled={busy || (!engaged && !canEngage) || (!engaged && !running)}
            loading={busy}
            aria-label={
              engaged ? t('gateway.aggregate.disable') : t('gateway.aggregate.enable')
            }
            onChange={(checked) => {
              void handleToggle(checked);
            }}
          />
        </div>
      </div>

      <div className={styles.fieldRow}>
        <div className={styles.fieldMeta}>
          <span className={styles.fieldLabel}>{t('gateway.aggregate.naming')}</span>
          <span className={styles.fieldHelp}>{namingHint}</span>
        </div>
        <div className={styles.fieldControl}>
          <select
            className={styles.select}
            value={displayedNaming}
            disabled={busy || strictGroups}
            aria-label={t('gateway.aggregate.naming')}
            onChange={(event) => {
              const nextNaming = event.currentTarget.value as GatewayAggregateNamingMode;
              setNaming(nextNaming);
              if (engaged && normalizedAliases && normalizedGroups && siteIds.length > 0) {
                void runEngage(
                  siteIds,
                  effectiveSeparator,
                  normalizedAliases,
                  nextNaming,
                  normalizedGroups,
                );
              }
            }}
          >
            <option value="site_model">{t('gateway.aggregate.namingSiteModel')}</option>
            <option value="model_at_site">{t('gateway.aggregate.namingModelAtSite')}</option>
            <option value="model_only">{t('gateway.aggregate.namingModelOnly')}</option>
          </select>
        </div>
      </div>

      <p className={styles.helper}>
        {strictGroups ? t('gateway.aggregate.groupsHint') : t('gateway.aggregate.modeHint')}
      </p>
      {!running ? <p className={styles.helper}>{t('gateway.aggregate.takeoverHint')}</p> : null}

      <div className={styles.fieldRow}>
        <div className={styles.fieldMeta}>
          <span className={styles.fieldLabel}>{t('gateway.aggregate.separator')}</span>
          <span className={styles.fieldHelp}>{separatorHint}</span>
        </div>
        <div className={styles.fieldControl}>
          <input
            className={styles.separatorInput}
            value={displayedSeparator}
            disabled={busy || strictGroups}
            placeholder={t('gateway.aggregate.separatorPlaceholder')}
            aria-label={t('gateway.aggregate.separator')}
            aria-invalid={strictGroups ? false : separatorError !== null}
            onChange={(event) => setSeparator(event.currentTarget.value)}
            onBlur={handleSeparatorCommit}
          />
        </div>
      </div>
      {separatorError && !strictGroups ? (
        <div className={styles.error} role="alert">
          {separatorError === 'empty'
            ? t('gateway.aggregate.separatorInvalidEmpty')
            : t('gateway.aggregate.separatorInvalidReserved')}
        </div>
      ) : null}

      {loadingSites ? (
        <div className={styles.loading}>
          <Loader2 size={14} className={styles.spin} aria-hidden="true" />
        </div>
      ) : candidates.length === 0 ? (
        <div className={styles.emptyState}>
          <span>{t('gateway.aggregate.noSites')}</span>
          <p>{t('gateway.aggregate.noSitesHint', { cli: t(`settings.gateway.cli.${cliKey}`) })}</p>
        </div>
      ) : (
        <>
          <div className={styles.listHeader}>
            <span className={styles.listTitle}>
              <Route size={12} aria-hidden="true" />
              {strictGroups
                ? t('gateway.aggregate.groupsCount', { count: groups.length })
                : t('gateway.aggregate.selectedCount', { count: siteIds.length })}
            </span>
            <div className={styles.listActions}>
              {strictGroups ? (
                <button
                  type="button"
                  className={styles.textButton}
                  disabled={busy}
                  onClick={handleUseLegacyAggregate}
                >
                  {t('gateway.aggregate.useLegacy')}
                </button>
              ) : siteIds.length > 0 ? (
                <button
                  type="button"
                  className={styles.textButton}
                  disabled={busy}
                  onClick={handleClearSelection}
                >
                  {t('gateway.aggregate.clearSelection')}
                </button>
              ) : (
                <button
                  type="button"
                  className={styles.textButton}
                  disabled={busy}
                  onClick={() => setSiteIds(candidates.map((candidate) => candidate.id))}
                >
                  {t('gateway.aggregate.selectAll')}
                </button>
              )}
              <button
                type="button"
                className={styles.textButton}
                disabled={busy}
                onClick={handleAddGroup}
              >
                <Plus size={13} aria-hidden="true" />
                {strictGroups
                  ? t('gateway.aggregate.addGroup')
                  : t('gateway.aggregate.enableGroups')}
              </button>
            </div>
          </div>

          {strictGroups ? (
            <div className={styles.groupList} aria-label={t('gateway.aggregate.groups')}>
              {groups.map((group) => {
                const groupCandidates = group.provider_ids
                  .map((siteId) =>
                    candidates.find((candidate) => candidate.id === siteId) ?? {
                      id: siteId,
                      name: t('gateway.aggregate.staleProvider'),
                    },
                  );
                const availableToAdd = candidates.filter(
                  (candidate) => !group.provider_ids.includes(candidate.id),
                );
                const groupIdInvalid = !validateGatewayAggregateGroupId(group.id);
                return (
                  <section className={styles.groupSection} key={group.draftKey}>
                    <div className={styles.groupHeader}>
                      <label className={styles.groupNameField}>
                        <span className={styles.srOnly}>{t('gateway.aggregate.groupId')}</span>
                        <input
                          className={styles.groupNameInput}
                          value={group.id}
                          disabled={busy}
                          maxLength={32}
                          aria-label={t('gateway.aggregate.groupId')}
                          aria-invalid={groupIdInvalid}
                          onChange={(event) =>
                            handleGroupIdChange(group.draftKey, event.currentTarget.value)
                          }
                          onBlur={handleGroupIdCommit}
                        />
                      </label>
                      <button
                        type="button"
                        className={styles.iconButton}
                        disabled={busy}
                        aria-label={`${t('gateway.aggregate.removeGroup')}: ${group.id}`}
                        title={t('gateway.aggregate.removeGroup')}
                        onClick={() => handleRemoveGroup(group.draftKey)}
                      >
                        <Trash2 size={13} aria-hidden="true" />
                      </button>
                    </div>
                    {groupIdInvalid ? (
                      <div className={styles.error} role="alert">
                        {t('gateway.aggregate.groupIdInvalid')}
                      </div>
                    ) : null}
                    <DndContext
                      sensors={busy ? [] : sensors}
                      collisionDetection={closestCenter}
                      modifiers={[restrictToVerticalAxis]}
                      onDragEnd={(event) => handleGroupDragEnd(group.draftKey, event)}
                    >
                      <SortableContext
                        items={group.provider_ids}
                        strategy={verticalListSortingStrategy}
                      >
                        <ul className={styles.siteList}>
                          {groupCandidates.map((candidate, index) => (
                            <SortableSiteRow
                              key={`${group.draftKey}:${candidate.id}`}
                              candidate={candidate}
                              index={index}
                              lastIndex={groupCandidates.length - 1}
                              onToggleSite={(siteId, checked) =>
                                handleToggleGroupSite(group.draftKey, siteId, checked)
                              }
                              onMoveSite={(siteId, direction) =>
                                handleMoveGroupSite(group.draftKey, siteId, direction)
                              }
                              disabled={busy}
                              alias={aliases[candidate.id] ?? ''}
                              onAliasChange={(siteId, alias) => {
                                const nextAliases = { ...aliases, [siteId]: alias };
                                if (!alias.trim()) delete nextAliases[siteId];
                                setAliases(nextAliases);
                              }}
                              onAliasCommit={handleAliasCommit}
                              aliasEditable={false}
                              groupId={group.id}
                            />
                          ))}
                        </ul>
                      </SortableContext>
                    </DndContext>
                    <div className={styles.groupAddRow}>
                      <label className={styles.groupAddLabel}>
                        <span>{t('gateway.aggregate.addSiteToGroup')}</span>
                        <select
                          className={styles.select}
                          value=""
                          disabled={busy || availableToAdd.length === 0}
                          aria-label={`${group.id}: ${t('gateway.aggregate.addSiteToGroup')}`}
                          onChange={(event) => {
                            const siteId = event.currentTarget.value;
                            if (siteId) handleToggleGroupSite(group.draftKey, siteId, true);
                          }}
                        >
                          <option value="">
                            {availableToAdd.length > 0
                              ? t('gateway.aggregate.chooseSite')
                              : t('gateway.aggregate.allSitesInGroup')}
                          </option>
                          {availableToAdd.map((candidate) => (
                            <option key={candidate.id} value={candidate.id}>
                              {candidate.name} ({candidate.id})
                            </option>
                          ))}
                        </select>
                      </label>
                    </div>
                    {group.provider_ids.length === 0 ? (
                      <div className={styles.error} role="alert">
                        {t('gateway.aggregate.groupProvidersRequired')}
                      </div>
                    ) : null}
                  </section>
                );
              })}
            </div>
          ) : (
            <>
          <DndContext
            sensors={busy ? [] : sensors}
            collisionDetection={closestCenter}
            modifiers={[restrictToVerticalAxis]}
            onDragEnd={handleDragEnd}
          >
            <SortableContext items={siteIds} strategy={verticalListSortingStrategy}>
              <ul className={styles.siteList}>
                {selectedCandidates.map((candidate, index) => (
                  <SortableSiteRow
                    key={candidate.id}
                    candidate={candidate}
                    index={index}
                    lastIndex={selectedCandidates.length - 1}
                    onToggleSite={handleToggleSite}
                    onMoveSite={handleMoveSite}
                    disabled={busy}
                    alias={aliases[candidate.id] ?? ''}
                    onAliasChange={(siteId, alias) => {
                      const nextAliases = { ...aliases, [siteId]: alias };
                      if (!alias.trim()) delete nextAliases[siteId];
                      setAliases(nextAliases);
                    }}
                    onAliasCommit={handleAliasCommit}
                  />
                ))}
              </ul>
            </SortableContext>
          </DndContext>

          {unselectedCandidates.length > 0 ? (
            <ul className={styles.siteList}>
              {unselectedCandidates.map((candidate) => (
                <li key={candidate.id} className={styles.siteItem}>
                  <input
                    type="checkbox"
                    checked={false}
                    disabled={busy}
                    aria-label={candidate.name}
                    onChange={(event) =>
                      handleToggleSite(candidate.id, event.currentTarget.checked)
                    }
                  />
                  <span className={styles.siteName} title={candidate.name}>
                    {candidate.name}
                  </span>
                  <code className={styles.siteSlug} title={candidate.id}>
                    {candidate.id}
                  </code>
                </li>
              ))}
            </ul>
          ) : null}
            </>
          )}
        </>
      )}

      {invalidSiteIds.length > 0 ? (
        <div className={styles.error} role="alert">
          {t('gateway.aggregate.notice.invalidConfig')}
        </div>
      ) : null}
      {staleSiteIds.length > 0 || staleGroupSiteIds.length > 0 ? (
        <div className={styles.error} role="alert">
          {t('gateway.aggregate.staleProviderHint', {
            providers: [...new Set([...staleSiteIds, ...staleGroupSiteIds])].join(', '),
          })}
        </div>
      ) : null}
      {notice ? (
        <div
          className={notice.kind === 'error' ? styles.error : styles.success}
          role="status"
        >
          {notice.text}
        </div>
      ) : null}
    </div>
  );
};

export default GatewayAggregateSettings;
