import type { GatewayAggregateGroup, GatewayAggregateNamingMode } from '@/services';
import {
  notifyGatewayAggregateConfigChanged,
  runGatewayAggregateMutation,
} from './gatewayAggregateMutation';

export type GatewayReengageMode = 'single' | 'failover' | 'aggregate' | null | undefined;

/** Aggregate routing config that must be replayed when re-engaging. */
export interface GatewayAggregateReengageConfig {
  providerIds: string[];
  separator: string;
  aliases?: Record<string, string>;
  naming?: GatewayAggregateNamingMode;
  /** Empty for legacy aggregate manifests; non-empty enables strict groups. */
  groups?: GatewayAggregateGroup[];
}

export interface GatewayReengageSnapshot {
  gatewayMode: GatewayReengageMode;
  aggregateConfig?: GatewayAggregateReengageConfig | null;
}

interface SaveProviderWithGatewayReengageOptions<TResult, TStatus> {
  gatewayMode: GatewayReengageMode;
  saveProvider: () => Promise<TResult>;
  restoreDirect: () => Promise<TStatus>;
  engageSingle: () => Promise<TStatus>;
  engageFailover: () => Promise<TStatus>;
  /** Required when `gatewayMode` is `aggregate`; ignored otherwise. */
  engageAggregate?: (
    config: GatewayAggregateReengageConfig,
  ) => Promise<TStatus>;
  /** Aggregate selection to replay. Required when `gatewayMode` is `aggregate`. */
  aggregateConfig?: GatewayAggregateReengageConfig | null;
  /**
   * Reads the current canonical takeover just after this operation reaches the
   * aggregate mutation lane. It prevents a provider save that waited behind a
   * newer aggregate edit from replaying an obsolete captured manifest.
   */
  resolveCurrentGatewayReengage?: () => Promise<GatewayReengageSnapshot>;
  onGatewayStatusChange?: (status: TStatus) => void;
}

export const isGatewayReengageMode = (
  gatewayMode: GatewayReengageMode,
): gatewayMode is 'single' | 'failover' | 'aggregate' =>
  gatewayMode === 'single' || gatewayMode === 'failover' || gatewayMode === 'aggregate';

export const saveProviderWithGatewayReengage = async <TResult, TStatus>({
  gatewayMode,
  saveProvider,
  restoreDirect,
  engageSingle,
  engageFailover,
  engageAggregate,
  aggregateConfig,
  resolveCurrentGatewayReengage,
  onGatewayStatusChange,
}: SaveProviderWithGatewayReengageOptions<TResult, TStatus>): Promise<TResult> => {
  if (!isGatewayReengageMode(gatewayMode)) {
    return saveProvider();
  }

  const saveAndReengage = async (): Promise<TResult> => {
    let effectiveGatewayMode: GatewayReengageMode = gatewayMode;
    let effectiveAggregateConfig = aggregateConfig;
    if (gatewayMode === 'aggregate' && resolveCurrentGatewayReengage) {
      const current = await resolveCurrentGatewayReengage();
      effectiveGatewayMode = current.gatewayMode;
      effectiveAggregateConfig = current.aggregateConfig ?? null;
    }
    if (!isGatewayReengageMode(effectiveGatewayMode)) {
      return saveProvider();
    }
    if (effectiveGatewayMode === 'aggregate' && (!engageAggregate || !effectiveAggregateConfig)) {
      throw new Error('Aggregate gateway re-engage requires engageAggregate and aggregateConfig');
    }

    const directStatus = await restoreDirect();
    onGatewayStatusChange?.(directStatus);

    const result = await saveProvider();

    // Aggregate re-engages with the current canonical site selection; the
    // caller owns the command so this helper stays free of service imports.
    // Falling back to single mode would silently drop the cross-site model
    // list, so require an explicit selection.
    if (effectiveGatewayMode === 'aggregate') {
      // Re-check after the awaited restore/save sequence so TypeScript retains
      // the same fail-closed narrowing at the actual invocation boundary.
      if (!engageAggregate || !effectiveAggregateConfig) {
        throw new Error('Aggregate gateway re-engage requires engageAggregate and aggregateConfig');
      }
      const aggregateStatus = await engageAggregate(effectiveAggregateConfig);
      onGatewayStatusChange?.(aggregateStatus);
      notifyGatewayAggregateConfigChanged();
      return result;
    }

    let nextStatus = await engageSingle();
    if (effectiveGatewayMode === 'failover') {
      nextStatus = await engageFailover();
    }
    onGatewayStatusChange?.(nextStatus);

    return result;
  };

  // A provider save while aggregate is active has the same manifest/catalog
  // write boundary as an editor change. Hold restore → save → re-engage as one
  // transaction-like lane so an aggregate edit cannot interleave halfway
  // through and be overwritten by an older snapshot.
  return gatewayMode === 'aggregate'
    ? runGatewayAggregateMutation(saveAndReengage)
    : saveAndReengage();
};
