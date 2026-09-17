import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

test('strict aggregate group drafts keep React identity separate from API payloads', async () => {
  const source = await readFile(
    new URL('../../../../../features/coding/gateway/components/GatewayAggregateSettings.tsx', import.meta.url),
    'utf8',
  );
  assert.match(source, /type GatewayAggregateGroupDraft = GatewayAggregateGroup &/);
  assert.match(source, /key=\{group\.draftKey\}/);

  const requestStart = source.indexOf('const requestGroups = nextGroups.map');
  const requestEnd = source.indexOf('return runGatewayOperation', requestStart);
  assert.notEqual(requestStart, -1, 'request group projection missing');
  assert.notEqual(requestEnd, -1, 'request group projection boundary missing');
  const requestProjection = source.slice(requestStart, requestEnd);
  assert.match(requestProjection, /id: group\.id/);
  assert.match(requestProjection, /provider_ids: \[\.\.\.group\.provider_ids\]/);
  assert.doesNotMatch(requestProjection, /draftKey/);
});
