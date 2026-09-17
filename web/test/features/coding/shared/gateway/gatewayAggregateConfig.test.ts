import assert from 'node:assert/strict';
import test from 'node:test';

import type { GatewayCliTakeoverStatus } from '../../../../../services/proxyGatewayApi.ts';
import {
  buildGatewayAggregateModelSlug,
  createLatestGatewayAggregateOperationQueue,
  defaultGatewayAggregateAlias,
  flattenGatewayAggregateGroups,
  getGatewayAggregateConfigVersion,
  isAggregateSiteId,
  notifyGatewayAggregateConfigChanged,
  normalizeGatewayAggregateAliases,
  normalizeGatewayAggregateGroups,
  normalizeGatewayAggregateSeparator,
  normalizeGatewayAggregateSiteIds,
  prepareGatewayAggregateAliasReengage,
  pruneStaleGatewayAggregateAliases,
  resolveGatewayReengageMode,
  runGatewayAggregateMutation,
  subscribeGatewayAggregateConfig,
  toGatewayAggregateReengageConfig,
  validateGatewayAggregateAlias,
  validateGatewayAggregateSeparator,
} from '../../../../../features/coding/shared/gateway/gatewayAggregateConfig.ts';
import {
  isGatewayProxyMode,
  isGatewayAggregateMode,
  isGatewayFailoverMode,
} from '../../../../../features/coding/shared/gateway/providerProtocol.ts';

const status = (
  partial: Partial<GatewayCliTakeoverStatus> & Pick<GatewayCliTakeoverStatus, 'mode'>,
): GatewayCliTakeoverStatus => ({
  cli_key: 'codex',
  state: 'takeover_applied',
  dot: 'green',
  can_takeover: true,
  can_restore_direct: true,
  gateway_origin: 'http://127.0.0.1:37124',
  runtime_root: null,
  managed_targets: [],
  primary_provider_id: null,
  provider_priorities: [],
  message: null,
  ...partial,
});

// ---- mode guards -----------------------------------------------------------

test('mode guards keep aggregate distinct from failover', () => {
  assert.equal(isGatewayProxyMode('single'), true);
  assert.equal(isGatewayProxyMode('failover'), true);
  assert.equal(isGatewayProxyMode('aggregate'), true);
  assert.equal(isGatewayProxyMode(null), false);
  assert.equal(isGatewayProxyMode(undefined), false);

  // Aggregate must never be reported as failover: the failover UI pins a
  // primary provider and shows P0/P1 priorities, neither of which exists here.
  assert.equal(isGatewayFailoverMode('aggregate'), false);
  assert.equal(isGatewayFailoverMode('failover'), true);
  assert.equal(isGatewayAggregateMode('failover'), false);
  assert.equal(isGatewayAggregateMode('aggregate'), true);
});

// ---- separator validation --------------------------------------------------

test('separator must be non-empty and free of every supported site-prefix character', () => {
  assert.equal(validateGatewayAggregateSeparator('.'), null);
  assert.equal(validateGatewayAggregateSeparator('::'), null);
  assert.equal(validateGatewayAggregateSeparator('/'), null);
  assert.equal(validateGatewayAggregateSeparator('.'), null);

  assert.equal(validateGatewayAggregateSeparator(''), 'empty');
  assert.equal(validateGatewayAggregateSeparator('a'), 'reservedCharacters');
  assert.equal(validateGatewayAggregateSeparator('9'), 'reservedCharacters');
  assert.equal(validateGatewayAggregateSeparator('_'), 'reservedCharacters');
  assert.equal(validateGatewayAggregateSeparator('-'), 'reservedCharacters');
  assert.equal(validateGatewayAggregateSeparator('思'), 'reservedCharacters');
  // Whitespace-only input follows the persisted-command trim semantics, so it
  // becomes an empty separator rather than a usable whitespace separator.
  assert.equal(validateGatewayAggregateSeparator(' '), 'empty');
  // One bad character inside an otherwise fine separator is still rejected.
  assert.equal(validateGatewayAggregateSeparator('.-'), 'reservedCharacters');
});

test('aliases accept safe Unicode provider names and default to them when no custom alias exists', () => {
  assert.equal(validateGatewayAggregateAlias('思辰888'), true);
  assert.equal(validateGatewayAggregateAlias('思源888 pro'), true);
  assert.equal(validateGatewayAggregateAlias(' relay_01-备用 '), true);
  assert.equal(validateGatewayAggregateAlias('unsafe.name'), false);
  assert.equal(validateGatewayAggregateAlias(''), false);
  assert.equal(validateGatewayAggregateAlias('a'.repeat(33)), false);

  assert.equal(defaultGatewayAggregateAlias(' 思辰888 '), '思辰888');
  assert.equal(defaultGatewayAggregateAlias('思源888 pro'), '思源888 pro');
  assert.equal(defaultGatewayAggregateAlias('unsafe.name'), null);
  assert.equal(defaultGatewayAggregateAlias('   '), null);
});

// ---- site ids --------------------------------------------------------------

test('site ids follow the backend addressable charset', () => {
  assert.equal(isAggregateSiteId('76a6ef74'), true);
  assert.equal(isAggregateSiteId('site-1'), true);
  assert.equal(isAggregateSiteId('site_1'), true);
  assert.equal(isAggregateSiteId(''), false);
  assert.equal(isAggregateSiteId('site.1'), false);
  assert.equal(isAggregateSiteId('site:1'), false);
  assert.equal(isAggregateSiteId('site 1'), false);
});

test('normalize keeps order, drops duplicates and unusable ids', () => {
  assert.deepEqual(
    normalizeGatewayAggregateSiteIds(['a', 'b', 'a', ' site:1 ', 'c', '']),
    ['a', 'b', 'c'],
  );
  assert.deepEqual(normalizeGatewayAggregateSiteIds([]), []);
});

test('alias validation includes unselected addressable fallback sites', () => {
  assert.equal(
    normalizeGatewayAggregateAliases(
      { 'site-a': 'site-b' },
      ['site-a'],
      ['site-a', 'site-b'],
    ),
    null,
  );
  assert.equal(
    normalizeGatewayAggregateAliases(
      { 'site-a': 'SITE-B' },
      ['site-a'],
      ['site-a', 'site-b'],
    ),
    null,
  );
  assert.deepEqual(
    normalizeGatewayAggregateAliases(
      { 'site-a': 'relay-a' },
      ['site-a'],
      ['site-a', 'site-b'],
    ),
    { 'site-a': 'relay-a' },
  );
});

test('alias normalization fails closed for malformed persisted values', () => {
  assert.equal(
    normalizeGatewayAggregateAliases(
      { 'site-a': 42 as unknown as string },
      ['site-a'],
      ['site-a'],
    ),
    null,
  );
});

test('stale alias pruning preserves saved aliases until addressable sites are known', () => {
  const aliases = {
    'site-a': 'relay-a',
    'stale-site': 'relay-stale',
  };
  assert.deepEqual(
    pruneStaleGatewayAggregateAliases(aliases, ['site-a', 'stale-site']),
    aliases,
  );
  assert.deepEqual(
    pruneStaleGatewayAggregateAliases(aliases, ['site-a', 'stale-site'], ['site-a']),
    { 'site-a': 'relay-a' },
  );
});

test('strict groups allow one provider to participate in multiple groups', () => {
  const groups = normalizeGatewayAggregateGroups(
    [
      { id: 'fast', provider_ids: ['site-a', 'site-b'] },
      { id: 'vision', provider_ids: ['site-a'] },
    ],
    ['site-a', 'site-b'],
  );
  assert.deepEqual(groups, [
    { id: 'fast', provider_ids: ['site-a', 'site-b'] },
    { id: 'vision', provider_ids: ['site-a'] },
  ]);
  assert.deepEqual(flattenGatewayAggregateGroups(groups), ['site-a', 'site-b']);
});

test('strict groups fail closed for stale providers and duplicate names', () => {
  assert.equal(
    normalizeGatewayAggregateGroups(
      [{ id: 'fast', provider_ids: ['site-a', 'missing-site'] }],
      ['site-a'],
    ),
    null,
  );
  assert.equal(
    normalizeGatewayAggregateGroups(
      [
        { id: 'fast', provider_ids: ['site-a'] },
        { id: 'FAST', provider_ids: ['site-b'] },
      ],
      ['site-a', 'site-b'],
    ),
    null,
  );
  assert.equal(
    normalizeGatewayAggregateGroups(
      [{ id: 'fast', provider_ids: ['site-a', 'site-a'] }],
      ['site-a', 'site-b'],
    ),
    null,
  );
});

test('separator normalization mirrors backend trim and empty fallback semantics', () => {
  assert.equal(normalizeGatewayAggregateSeparator('  ::  '), '::');
  assert.equal(normalizeGatewayAggregateSeparator('\t.\n'), '.');
  assert.equal(normalizeGatewayAggregateSeparator(' \t '), '.');
  assert.equal(validateGatewayAggregateSeparator('  ::  '), null);
  assert.equal(validateGatewayAggregateSeparator('  a  '), 'reservedCharacters');
  assert.equal(validateGatewayAggregateSeparator(' \t '), 'empty');
});

test('latest aggregate operation queue serializes commands and marks stale work', async () => {
  const queue = createLatestGatewayAggregateOperationQueue();
  const events: string[] = [];
  let releaseFirst!: () => void;
  const firstGate = new Promise<void>((resolve) => {
    releaseFirst = resolve;
  });
  let resolveFirstStarted!: () => void;
  const firstStarted = new Promise<void>((resolve) => {
    resolveFirstStarted = resolve;
  });

  const first = queue.enqueue(async (isCurrent) => {
    events.push('first:start');
    resolveFirstStarted();
    await firstGate;
    if (!isCurrent()) {
      events.push('first:stale');
      return 'stale';
    }
    events.push('first:apply');
    return 'applied';
  });
  await firstStarted;
  const second = queue.enqueue(async () => {
    events.push('second:apply');
    return 'applied';
  });

  releaseFirst();
  assert.equal(await first, 'stale');
  assert.equal(await second, 'applied');
  assert.deepEqual(events, ['first:start', 'first:stale', 'second:apply']);
});

test('aggregate editor instances share one backend mutation lane and receive canonical refresh notifications', async () => {
  const events: string[] = [];
  let releaseFirst!: () => void;
  const firstGate = new Promise<void>((resolve) => {
    releaseFirst = resolve;
  });
  const notificationVersions: number[] = [];
  const unsubscribe = subscribeGatewayAggregateConfig(() => {
    notificationVersions.push(getGatewayAggregateConfigVersion());
  });
  const beforeVersion = getGatewayAggregateConfigVersion();

  const first = runGatewayAggregateMutation(async () => {
    events.push('first:start');
    await firstGate;
    events.push('first:end');
    return 'first';
  });
  const second = runGatewayAggregateMutation(async () => {
    events.push('second:start');
    events.push('second:end');
    return 'second';
  });

  await Promise.resolve();
  assert.deepEqual(events, ['first:start']);
  releaseFirst();
  assert.equal(await first, 'first');
  assert.equal(await second, 'second');
  assert.deepEqual(events, ['first:start', 'first:end', 'second:start', 'second:end']);

  notifyGatewayAggregateConfigChanged();
  unsubscribe();
  assert.deepEqual(notificationVersions, [beforeVersion + 1]);
});

// ---- re-engage resolution --------------------------------------------------

test('re-engage mode only accepts aggregate when its selection is complete', () => {
  assert.equal(resolveGatewayReengageMode(status({ mode: 'single' })), 'single');
  assert.equal(resolveGatewayReengageMode(status({ mode: 'failover' })), 'failover');
  assert.equal(resolveGatewayReengageMode(status({ mode: null })), null);
  assert.equal(resolveGatewayReengageMode(null), null);
  assert.equal(resolveGatewayReengageMode(undefined), null);

  // Aggregate without manifest details must not re-engage: engaging with an
  // empty site list would silently drop the whole cross-site model list.
  assert.equal(resolveGatewayReengageMode(status({ mode: 'aggregate' })), null);
  assert.equal(
    resolveGatewayReengageMode(
      status({ mode: 'aggregate', aggregate: { provider_ids: [], separator: '.' } }),
    ),
    null,
  );
  assert.equal(
    resolveGatewayReengageMode(
      status({ mode: 'aggregate', aggregate: { provider_ids: ['site1'], separator: '-' } }),
    ),
    null,
  );
  assert.equal(
    resolveGatewayReengageMode(
      status({ mode: 'aggregate', aggregate: { provider_ids: ['site1'], separator: '.' } }),
    ),
    'aggregate',
  );
});

test('aggregate reengage config is only built for a valid aggregate manifest', () => {
  assert.equal(toGatewayAggregateReengageConfig(status({ mode: 'single' })), null);
  // An invalid separator cannot be replayed: the backend would reject it.
  assert.equal(
    toGatewayAggregateReengageConfig(
      status({ mode: 'aggregate', aggregate: { provider_ids: ['b', 'a'], separator: '-' } }),
    ),
    null,
  );
  assert.deepEqual(
    toGatewayAggregateReengageConfig(
      status({
        mode: 'aggregate',
        aggregate: { provider_ids: ['b', 'a', 'b'], separator: '::' },
      }),
    ),
    { providerIds: ['b', 'a'], separator: '::', aliases: {}, naming: 'site_model', groups: [] },
  );
  assert.equal(
    toGatewayAggregateReengageConfig(
      status({
        mode: 'aggregate',
        aggregate: {
          provider_ids: ['site-a'],
          separator: '.',
          aliases: { 'site-a': 'site-b' },
        },
      }),
      ['site-a', 'site-b'],
    ),
    null,
  );
  assert.equal(
    toGatewayAggregateReengageConfig(
      status({
        mode: 'aggregate',
        aggregate: {
          provider_ids: ['site-a'],
          separator: ' . ',
          aliases: { 'site-a': 'site-b' },
        },
        provider_priorities: [
          { provider_id: 'site-a', label: 'P0' },
          { provider_id: 'site-b', label: 'P1' },
        ],
      }),
    ),
    null,
  );
  assert.deepEqual(
    toGatewayAggregateReengageConfig(
      status({
        mode: 'aggregate',
        aggregate: { provider_ids: ['site-a'], separator: ' . ' },
      }),
    ),
    { providerIds: ['site-a'], separator: '.', aliases: {}, naming: 'site_model', groups: [] },
  );
  assert.deepEqual(
    toGatewayAggregateReengageConfig(
      status({
        mode: 'aggregate',
        aggregate: {
          // Strict compatibility provider_ids are canonicalized from group
          // order rather than trusting a stale/legacy flat ordering.
          provider_ids: ['site-b', 'site-a'],
          separator: '-',
          aliases: { 'site-a': 'site-b' },
          naming: 'model_only',
          groups: [
            { id: 'general', provider_ids: ['site-a', 'site-b'] },
            { id: 'vision', provider_ids: ['site-a'] },
          ],
        },
      }),
    ),
    {
      providerIds: ['site-a', 'site-b'],
      separator: '.',
      aliases: {},
      naming: 'site_model',
      groups: [
        { id: 'general', provider_ids: ['site-a', 'site-b'] },
        { id: 'vision', provider_ids: ['site-a'] },
      ],
    },
  );
  assert.equal(
    toGatewayAggregateReengageConfig(
      status({
        mode: 'aggregate',
        aggregate: {
          provider_ids: ['site-a'],
          separator: '.',
          groups: [{ id: 'empty', provider_ids: [] }],
        },
      }),
    ),
    null,
  );
});

test('aggregate slug joins site id and model with the configured separator', () => {
  assert.equal(buildGatewayAggregateModelSlug('76a6ef74', 'deepseek-v4-flash', '.'), '76a6ef74.deepseek-v4-flash');
  assert.equal(buildGatewayAggregateModelSlug('site-1', 'glm-5.3', '::'), 'site-1::glm-5.3');
});

test('alias edits produce an immediate re-engage payload only when valid', () => {
  assert.deepEqual(
    prepareGatewayAggregateAliasReengage(
      {},
      'site-a',
      'relay-a',
      ['site-a', 'site-b'],
      true,
      '.',
      'site_model',
    ),
    {
      siteIds: ['site-a', 'site-b'],
      separator: '.',
      aliases: { 'site-a': 'relay-a' },
      naming: 'site_model',
    },
  );
  assert.equal(
    prepareGatewayAggregateAliasReengage(
      {},
      'site-a',
      'bad.alias',
      ['site-a'],
      true,
      '.',
      'site_model',
    ),
    null,
  );
  assert.equal(
    prepareGatewayAggregateAliasReengage(
      {},
      'site-a',
      'relay-a',
      ['site-a'],
      false,
      '.',
      'site_model',
    ),
    null,
  );
});
