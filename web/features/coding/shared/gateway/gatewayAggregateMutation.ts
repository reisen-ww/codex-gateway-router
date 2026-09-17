/**
 * Process-wide coordination for mutations that rewrite the aggregate gateway
 * manifest and Codex model catalog.
 *
 * This module intentionally has no service or component imports. Both the
 * aggregate editors and provider-save re-engage flow use it, avoiding a
 * circular dependency while keeping backend writes serialized.
 */

let aggregateMutationTail: Promise<void> = Promise.resolve();

export const runGatewayAggregateMutation = <T>(
  mutation: () => Promise<T>,
): Promise<T> => {
  const run = aggregateMutationTail.then(mutation);
  aggregateMutationTail = run.then(
    () => undefined,
    () => undefined,
  );
  return run;
};

// Every successful aggregate mutation notifies mounted editors to reload the
// backend manifest. KeepAlive instances must never keep an editable stale
// draft after a different entry point has changed the canonical configuration.
let aggregateConfigVersion = 0;
const aggregateConfigListeners = new Set<() => void>();

export const getGatewayAggregateConfigVersion = () => aggregateConfigVersion;

export const subscribeGatewayAggregateConfig = (listener: () => void) => {
  aggregateConfigListeners.add(listener);
  return () => {
    aggregateConfigListeners.delete(listener);
  };
};

export const notifyGatewayAggregateConfigChanged = () => {
  aggregateConfigVersion += 1;
  for (const listener of aggregateConfigListeners) {
    listener();
  }
};
