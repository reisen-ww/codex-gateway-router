import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

const pageContracts = [
  {
    name: 'Claude Code',
    cliKey: 'claude',
    path: '../../../../../features/coding/claudecode/pages/ClaudeCodePage.tsx',
  },
  {
    name: 'Claude Desktop',
    cliKey: 'claude_desktop',
    path: '../../../../../features/coding/claudedesktop/pages/ClaudeDesktopPage.tsx',
  },
  {
    name: 'Codex',
    cliKey: 'codex',
    path: '../../../../../features/coding/codex/pages/CodexPage.tsx',
  },
  {
    name: 'Gemini CLI',
    cliKey: 'gemini',
    path: '../../../../../features/coding/geminicli/pages/GeminiCliPage.tsx',
  },
  {
    name: 'Grok',
    cliKey: 'grok',
    path: '../../../../../features/coding/grok/pages/GrokPage.tsx',
  },
] as const;

test('CLI config loaders reject stale responses and refresh takeover status', async () => {
  for (const contract of pageContracts) {
    const source = await readFile(
      new URL(contract.path, import.meta.url),
      'utf8',
    );
    const loadStart = source.indexOf('const loadConfig =');
    const effectStart = source.indexOf('React.useEffect', loadStart);
    assert.notEqual(loadStart, -1, `${contract.name}: loadConfig missing`);
    assert.notEqual(effectStart, -1, `${contract.name}: loadConfig boundary missing`);
    const loadBlock = source.slice(loadStart, effectStart);

    assert.match(
      loadBlock,
      /const requestId = \+\+loadConfigRequestIdRef\.current;/,
      `${contract.name}: request id missing`,
    );
    assert.match(
      loadBlock,
      /if \(requestId !== loadConfigRequestIdRef\.current\) return;/,
      `${contract.name}: stale response guard missing`,
    );
    if (contract.cliKey === 'codex') {
      // Codex has an additional aggregate quick-entry refresh path, so its
      // Gateway status uses a dedicated request generation shared by config
      // loads and aggregate mutations. This is stricter than tying status to a
      // single loadConfig invocation: an old config load cannot overwrite a
      // newer aggregate result.
      assert.match(
        loadBlock,
        /void refreshGatewayCliStatus\(\)\.catch\(\(\) => \{\}\);/,
        `${contract.name}: Gateway status refresh is not delegated`,
      );
      assert.match(
        source,
        /const refreshGatewayCliStatus = React\.useCallback\(async \(\) => \{[\s\S]*?gatewayCliStatusRequestRef\.current = request;[\s\S]*?getProxyGatewayCliStatus\('codex'\)[\s\S]*?gatewayCliStatusRequestRef\.current === request/,
        `${contract.name}: Gateway status refresh is not request-scoped`,
      );
    } else {
      assert.match(
        loadBlock,
        new RegExp(
          String.raw`getProxyGatewayCliStatus\('${contract.cliKey}'\)[\s\S]*?if \(requestId !== loadConfigRequestIdRef\.current\) return;`,
        ),
        `${contract.name}: Gateway status refresh is not request-scoped`,
      );
    }
    assert.match(
      loadBlock,
      /catch \(error\) \{[\s\S]*?if \(requestId !== loadConfigRequestIdRef\.current\) return;/,
      `${contract.name}: stale error guard missing`,
    );
    assert.match(
      loadBlock,
      /finally \{[\s\S]*?if \(requestId === loadConfigRequestIdRef\.current\) \{\s*setLoading\(false\);/,
      `${contract.name}: stale loading cleanup guard missing`,
    );
  }
});
