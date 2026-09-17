import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

const codexPageSource = async () =>
  readFile(
    new URL('../../../../../features/coding/codex/pages/CodexPage.tsx', import.meta.url),
    'utf8',
  );

test('Codex shows a separate aggregate-mode entry beside failover for every active Gateway takeover mode', async () => {
  const source = await codexPageSource();

  assert.match(
    source,
    /const gatewayProxyModeActive\s*=\s*isGatewayProxyMode\(gatewayCliStatus\?\.mode\)/,
  );
  assert.match(source, /\{gatewayProxyModeActive \? \(/);
  assert.match(source, /gateway\.aggregate\.button/);
  assert.match(source, /onClick=\{handleOpenAggregateSettings\}/);
});

test('Codex aggregate-mode entry opens the reusable configuration drawer instead of an instructional guide', async () => {
  const source = await codexPageSource();

  const handlerStart = source.indexOf('const handleOpenAggregateSettings');
  const handlerEnd = source.indexOf('const [savingCodexUnifiedHistory', handlerStart);
  assert.notEqual(handlerStart, -1, 'aggregate settings handler is missing');
  assert.notEqual(handlerEnd, -1, 'aggregate settings handler boundary is missing');

  const handler = source.slice(handlerStart, handlerEnd);
  assert.match(handler, /getProxyGatewayStatus\(\)/);
  assert.match(handler, /setAggregateGatewayRunning\(status\.running\)/);
  assert.match(handler, /setAggregateSettingsOpen\(true\)/);
  assert.doesNotMatch(handler, /Modal\.info\(/);

  assert.match(source, /<Drawer[\s\S]*destroyOnHidden/);
  assert.match(source, /<GatewayAggregateSettings[\s\S]*running=\{aggregateGatewayRunning\}/);
  assert.match(source, /onTakeoverChange=\{handleAggregateTakeoverChange\}/);
});

test('Codex aggregate drawer is lifecycle-safe in a KeepAlive page', async () => {
  const source = await codexPageSource();

  assert.match(
    source,
    /if \(isActive\) \{\s*return;\s*\}\s*aggregateSettingsRequestRef\.current \+= 1;\s*aggregateGatewayStatusRequestRef\.current \+= 1;\s*setAggregateSettingsOpening\(false\);\s*setAggregateSettingsOpen\(false\);/,
  );
  assert.match(source, /listen<boolean>\('gateway-running-changed'/);
  assert.match(source, /const refreshGatewayCliStatus = React\.useCallback/);
  assert.match(source, /gatewayCliStatusRequestRef\.current === request/);
});

test('Codex keeps the aggregate settings shortcut distinct from the takeover-status control', async () => {
  const source = await readFile(
    new URL('../../../../../features/coding/shared/gateway/GatewayFailoverButton.tsx', import.meta.url),
    'utf8',
  );

  assert.match(source, /aggregateActive\s*\?\s*t\('gateway\.takeover\.statusButton'\)/);
  assert.doesNotMatch(source, /aggregateActive\s*\?\s*t\('gateway\.aggregate\.button'\)/);
});
