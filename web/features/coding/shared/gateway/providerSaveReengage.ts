import type { GatewayAggregateGroup, GatewayAggregateNamingMode } from '@/services';

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
  onGatewayStatusChange,
}: SaveProviderWithGatewayReengageOptions<TResult, TStatus>): Promise<TResult> => {
  if (!isGatewayReengageMode(gatewayMode)) {
    return saveProvider();
  }

  const directStatus = await restoreDirect();
  onGatewayStatusChange?.(directStatus);

  const result = await saveProvider();

  // Aggregate re-engages with the captured site selection; the caller owns the
  // command so this helper stays free of service imports. Falling back to
  // single mode would silently drop the cross-site model list, so require it.
  if (gatewayMode === 'aggregate') {
    if (!engageAggregate || !aggregateConfig) {
      throw new Error('Aggregate gateway re-engage requires engageAggregate and aggregateConfig');
    }
    const aggregateStatus = await engageAggregate(aggregateConfig);
    onGatewayStatusChange?.(aggregateStatus);
    return result;
  }

  let nextStatus = await engageSingle();
  if (gatewayMode === 'failover') {
    nextStatus = await engageFailover();
  }
  onGatewayStatusChange?.(nextStatus);

  return result;
};
