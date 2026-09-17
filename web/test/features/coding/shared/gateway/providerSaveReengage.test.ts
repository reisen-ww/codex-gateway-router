import assert from 'node:assert/strict';
import test from 'node:test';

import { saveProviderWithGatewayReengage } from '../../../../../features/coding/shared/gateway/providerSaveReengage.ts';
import {
  getGatewayAggregateConfigVersion,
  runGatewayAggregateMutation,
} from '../../../../../features/coding/shared/gateway/gatewayAggregateMutation.ts';

test('save provider reengage helper saves directly when gateway mode is inactive', async () => {
  const calls: string[] = [];

  const result = await saveProviderWithGatewayReengage({
    gatewayMode: null,
    saveProvider: async () => {
      calls.push('save');
      return 'saved';
    },
    restoreDirect: async () => {
      calls.push('restore');
      return 'direct';
    },
    engageSingle: async () => {
      calls.push('single');
      return 'single';
    },
    engageFailover: async () => {
      calls.push('failover');
      return 'failover';
    },
    onGatewayStatusChange: (status) => {
      calls.push(`status:${status}`);
    },
  });

  assert.equal(result, 'saved');
  assert.deepEqual(calls, ['save']);
});

test('save provider reengage helper restores direct before saving and reengages single mode', async () => {
  const calls: string[] = [];

  const result = await saveProviderWithGatewayReengage({
    gatewayMode: 'single',
    saveProvider: async () => {
      calls.push('save');
      return 'saved';
    },
    restoreDirect: async () => {
      calls.push('restore');
      return 'direct';
    },
    engageSingle: async () => {
      calls.push('single');
      return 'single';
    },
    engageFailover: async () => {
      calls.push('failover');
      return 'failover';
    },
    onGatewayStatusChange: (status) => {
      calls.push(`status:${status}`);
    },
  });

  assert.equal(result, 'saved');
  assert.deepEqual(calls, ['restore', 'status:direct', 'save', 'single', 'status:single']);
});

test('save provider reengage helper restores direct before saving and reengages failover mode', async () => {
  const calls: string[] = [];

  const result = await saveProviderWithGatewayReengage({
    gatewayMode: 'failover',
    saveProvider: async () => {
      calls.push('save');
      return 'saved';
    },
    restoreDirect: async () => {
      calls.push('restore');
      return 'direct';
    },
    engageSingle: async () => {
      calls.push('single');
      return 'single';
    },
    engageFailover: async () => {
      calls.push('failover');
      return 'failover';
    },
    onGatewayStatusChange: (status) => {
      calls.push(`status:${status}`);
    },
  });

  assert.equal(result, 'saved');
  assert.deepEqual(calls, [
    'restore',
    'status:direct',
    'save',
    'single',
    'failover',
    'status:failover',
  ]);
});

test('save provider reengage helper does not reengage when save fails after restore', async () => {
  const calls: string[] = [];

  await assert.rejects(
    saveProviderWithGatewayReengage({
      gatewayMode: 'single',
      saveProvider: async () => {
        calls.push('save');
        throw new Error('save failed');
      },
      restoreDirect: async () => {
        calls.push('restore');
        return 'direct';
      },
      engageSingle: async () => {
        calls.push('single');
        return 'single';
      },
      engageFailover: async () => {
        calls.push('failover');
        return 'failover';
      },
      onGatewayStatusChange: (status) => {
        calls.push(`status:${status}`);
      },
    }),
    /save failed/,
  );

  assert.deepEqual(calls, ['restore', 'status:direct', 'save']);
});

test('save provider reengage helper replays the aggregate selection, not single mode', async () => {
  const calls: string[] = [];

  const result = await saveProviderWithGatewayReengage({
    gatewayMode: 'aggregate',
    aggregateConfig: { providerIds: ['site-b', 'site-a'], separator: '.' },
    saveProvider: async () => {
      calls.push('save');
      return 'saved';
    },
    restoreDirect: async () => {
      calls.push('restore');
      return 'direct';
    },
    engageSingle: async () => {
      calls.push('single');
      return 'single';
    },
    engageFailover: async () => {
      calls.push('failover');
      return 'failover';
    },
    engageAggregate: async (config) => {
      calls.push(`aggregate:${config.providerIds.join(',')}:${config.separator}`);
      return 'aggregate';
    },
    onGatewayStatusChange: (status) => {
      calls.push(`status:${status}`);
    },
  });

  assert.equal(result, 'saved');
  // Aggregate must not fall through to single/failover: doing so would drop the
  // cross-site model list the user configured.
  assert.deepEqual(calls, [
    'restore',
    'status:direct',
    'save',
    'aggregate:site-b,site-a:.',
    'status:aggregate',
  ]);
});

test('save provider reengage helper forwards strict groups unchanged', async () => {
  const calls: string[] = [];

  await saveProviderWithGatewayReengage({
    gatewayMode: 'aggregate',
    aggregateConfig: {
      providerIds: ['site-a', 'site-b'],
      separator: '.',
      groups: [
        { id: 'general', provider_ids: ['site-a', 'site-b'] },
        { id: 'vision', provider_ids: ['site-a'] },
      ],
    },
    saveProvider: async () => {
      calls.push('save');
      return 'saved';
    },
    restoreDirect: async () => {
      calls.push('restore');
      return 'direct';
    },
    engageSingle: async () => 'single',
    engageFailover: async () => 'failover',
    engageAggregate: async (config) => {
      calls.push(JSON.stringify(config.groups));
      return 'aggregate';
    },
  });

  assert.deepEqual(calls, [
    'restore',
    'save',
    JSON.stringify([
      { id: 'general', provider_ids: ['site-a', 'site-b'] },
      { id: 'vision', provider_ids: ['site-a'] },
    ]),
  ]);
});

test('save provider reengage helper refuses aggregate without a selection', async () => {
  const calls: string[] = [];

  await assert.rejects(
    saveProviderWithGatewayReengage({
      gatewayMode: 'aggregate',
      aggregateConfig: null,
      saveProvider: async () => {
        calls.push('save');
        return 'saved';
      },
      restoreDirect: async () => {
        calls.push('restore');
        return 'direct';
      },
      engageSingle: async () => {
        calls.push('single');
        return 'single';
      },
      engageFailover: async () => {
        calls.push('failover');
        return 'failover';
      },
      engageAggregate: async () => {
        calls.push('aggregate');
        return 'aggregate';
      },
      onGatewayStatusChange: (status) => {
        calls.push(`status:${status}`);
      },
    }),
    /Aggregate gateway re-engage requires/,
  );

  assert.deepEqual(calls, []);
});

test('save provider reengage helper still reengages aggregate when the save fails', async () => {
  const calls: string[] = [];

  await assert.rejects(
    saveProviderWithGatewayReengage({
      gatewayMode: 'aggregate',
      aggregateConfig: { providerIds: ['site-a'], separator: '::' },
      saveProvider: async () => {
        calls.push('save');
        throw new Error('save failed');
      },
      restoreDirect: async () => {
        calls.push('restore');
        return 'direct';
      },
      engageSingle: async () => {
        calls.push('single');
        return 'single';
      },
      engageFailover: async () => {
        calls.push('failover');
        return 'failover';
      },
      engageAggregate: async () => {
        calls.push('aggregate');
        return 'aggregate';
      },
      onGatewayStatusChange: (status) => {
        calls.push(`status:${status}`);
      },
    }),
    /save failed/,
  );

  assert.deepEqual(calls, ['restore', 'status:direct', 'save']);
});

test('aggregate provider save waits for prior aggregate edits and re-reads the canonical config', async () => {
  const calls: string[] = [];
  let releasePriorMutation!: () => void;
  const priorMutationGate = new Promise<void>((resolve) => {
    releasePriorMutation = resolve;
  });
  let markPriorMutationStarted!: () => void;
  const priorMutationStarted = new Promise<void>((resolve) => {
    markPriorMutationStarted = resolve;
  });
  const beforeVersion = getGatewayAggregateConfigVersion();

  const priorMutation = runGatewayAggregateMutation(async () => {
    calls.push('drawer:start');
    markPriorMutationStarted();
    await priorMutationGate;
    calls.push('drawer:end');
  });
  await priorMutationStarted;

  const providerSave = saveProviderWithGatewayReengage({
    gatewayMode: 'aggregate',
    // This was captured before the drawer edit and must not be replayed.
    aggregateConfig: { providerIds: ['old-site'], separator: '.' },
    resolveCurrentGatewayReengage: async () => {
      calls.push('read-current');
      return {
        gatewayMode: 'aggregate',
        aggregateConfig: { providerIds: ['new-site'], separator: '::' },
      };
    },
    restoreDirect: async () => {
      calls.push('restore');
      return 'direct';
    },
    saveProvider: async () => {
      calls.push('save');
      return 'saved';
    },
    engageSingle: async () => {
      calls.push('single');
      return 'single';
    },
    engageFailover: async () => {
      calls.push('failover');
      return 'failover';
    },
    engageAggregate: async (config) => {
      calls.push(`aggregate:${config.providerIds.join(',')}:${config.separator}`);
      return 'aggregate';
    },
  });

  await Promise.resolve();
  assert.deepEqual(calls, ['drawer:start']);
  releasePriorMutation();
  await priorMutation;
  assert.equal(await providerSave, 'saved');
  assert.deepEqual(calls, [
    'drawer:start',
    'drawer:end',
    'read-current',
    'restore',
    'save',
    'aggregate:new-site:::',
  ]);
  assert.equal(getGatewayAggregateConfigVersion(), beforeVersion + 1);
});
