import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

import {
  assertAllowlistNotWideOpen,
  extractAllowlistPatterns,
  extractHttpUrlLiterals,
  extractOpenedUrls,
  isPatternWideOpen,
  isUrlAllowed,
  rustGlobMatches,
  urlSlashVariants,
} from './opener-allowlist.mjs';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

const PRODUCT_URLS = [
  'https://cubbyclipboard.com',
  'https://cubbyclipboard.com/',
  'https://cubbyclipboard.com/privacy',
  'https://www.cubbyclipboard.com',
  'https://www.cubbyclipboard.com/',
  'https://www.cubbyclipboard.com/privacy',
  'https://github.com/btsouth/cubby-clipboard',
];

const BLOCKED_URLS = [
  'https://evil.example',
  'https://github.com/other/repo',
  'https://cubbyclipboard.com.evil.com',
  'https://notcubbyclipboard.com/privacy',
];

const GITHUB_ONLY = ['https://github.com/btsouth/cubby-clipboard'];

test('rust glob star and question-mark cross slash, matching Pattern::matches defaults', () => {
  // SBS-997: the previous assertion that * / ? do not cross slash modeled
  // MatchOptions { require_literal_separator: true }. tauri-plugin-opener
  // calls glob::Pattern::matches, which uses MatchOptions::new() where that
  // flag is false, so * and ? match `/`. Keeping the old assertion would
  // hide a dropped-slash host pattern matching an attacker URL.
  assert.equal(rustGlobMatches('https://example.com/*', 'https://example.com/privacy'), true);
  assert.equal(rustGlobMatches('https://example.com/*', 'https://example.com/'), true);
  assert.equal(rustGlobMatches('https://example.com/*', 'https://example.com'), false);
  assert.equal(rustGlobMatches('https://example.com/*', 'https://example.com/a/b'), true);
  assert.equal(rustGlobMatches('https://example.com', 'https://example.com'), true);
  assert.equal(rustGlobMatches('https://example.com', 'https://example.com/'), false);
  assert.equal(rustGlobMatches('https://ex?mple.com', 'https://example.com'), true);
  assert.equal(rustGlobMatches('https://ex?mple.com', 'https://ex/mple.com'), true);
  assert.equal(
    rustGlobMatches('https://cubbyclipboard.com*', 'https://cubbyclipboard.com.evil.tld/phish'),
    true,
  );
  assert.equal(rustGlobMatches('https://example.com/a[bc]', 'https://example.com/ab'), true);
  assert.equal(rustGlobMatches('https://example.com/a[bc]', 'https://example.com/ad'), false);
  assert.equal(rustGlobMatches('https://example.com/[!a]', 'https://example.com/b'), true);
  assert.equal(rustGlobMatches('https://example.com/[!a]', 'https://example.com/a'), false);
});

test('live allowlist opens product website and privacy, including slash and www', async () => {
  const capability = JSON.parse(
    await readFile(path.join(repoRoot, 'src-tauri/capabilities/default.json'), 'utf8')
  );
  const patterns = extractAllowlistPatterns(capability);
  assertAllowlistNotWideOpen(patterns);
  for (const url of PRODUCT_URLS) {
    assert.equal(isUrlAllowed(url, patterns), true, url);
  }
  for (const url of BLOCKED_URLS) {
    assert.equal(isUrlAllowed(url, patterns), false, url);
  }
});

test('github-only allowlist still blocks the product website and privacy', () => {
  for (const url of [
    'https://cubbyclipboard.com',
    'https://cubbyclipboard.com/',
    'https://cubbyclipboard.com/privacy',
    'https://www.cubbyclipboard.com',
  ]) {
    assert.equal(isUrlAllowed(url, GITHUB_ONLY), false, url);
  }
  assert.equal(isUrlAllowed('https://github.com/btsouth/cubby-clipboard', GITHUB_ONLY), true);
});

test('host-wildcard patterns are treated as opening the entire web', () => {
  assert.equal(isPatternWideOpen('https://*'), true);
  assert.equal(isPatternWideOpen('http://*'), true);
  assert.equal(isPatternWideOpen('*'), true);
  assert.equal(isPatternWideOpen('**'), true);
  assert.equal(isPatternWideOpen('https://'), true);
  assert.equal(isPatternWideOpen('https://cubbyclipboard.com/*'), false);
  assert.equal(isPatternWideOpen('https://cubbyclipboard.com'), false);
  assert.equal(isPatternWideOpen('https://github.com/btsouth/cubby-clipboard/*'), false);
  assert.throws(() => assertAllowlistNotWideOpen(['https://*']), /entire web/);
});

test('authority wildcards are wide open, including a dropped-slash host glob', () => {
  // SBS-997: isPatternWideOpen used to inspect only an exactly-wildcard host
  // between :// and the first /. `https://cubbyclipboard.com*` has no slash,
  // so that check called the host `cubbyclipboard.com*` and accepted it.
  // At runtime glob's default * swallows `.evil.tld/phish`.
  const droppedSlash = 'https://cubbyclipboard.com*';
  assert.equal(
    isUrlAllowed('https://cubbyclipboard.com.evil.tld/phish', [droppedSlash]),
    true,
  );
  assert.equal(isPatternWideOpen(droppedSlash), true);
  assert.throws(() => assertAllowlistNotWideOpen([droppedSlash]), /entire web/);
  assert.equal(isPatternWideOpen('https://*.cubbyclipboard.com/'), true);
  assert.equal(isPatternWideOpen('https://cubbyclipboard.com?'), true);
  assert.equal(isPatternWideOpen('https://cubbyclipboard.com[.]evil.com'), true);
  assert.equal(isPatternWideOpen('https*://cubbyclipboard.com/*'), true);
  assert.throws(
    () => assertAllowlistNotWideOpen(['https://cubbyclipboard.com/*', droppedSlash]),
    /entire web/,
  );
});

test('Settings const URL assignments reach the allowlist check', () => {
  // check-release runs extractHttpUrlLiterals over SettingsPanel.tsx, so this
  // is the layer that has to see them. extractOpenedUrls deliberately does not
  // -- it runs over all of frontend/src, where the same shape is test fixtures.
  const source = [
    "const GITHUB_URL = 'https://github.com/btsouth/cubby-clipboard';",
    "const WEBSITE_URL = 'https://cubbyclipboard.com';",
    "const PRIVACY_URL = 'https://cubbyclipboard.com/privacy';",
  ].join('\n');
  assert.deepEqual(extractHttpUrlLiterals(source), [
    'https://github.com/btsouth/cubby-clipboard',
    'https://cubbyclipboard.com',
    'https://cubbyclipboard.com/privacy',
  ]);
});

test('slash variants cover the live homepage with and without a trailing slash', () => {
  assert.deepEqual(urlSlashVariants('https://cubbyclipboard.com'), [
    'https://cubbyclipboard.com',
    'https://cubbyclipboard.com/',
  ]);
});

test('path URLs keep their written form; rust-url does not add a slash', () => {
  assert.deepEqual(urlSlashVariants('https://cubbyclipboard.com/privacy'), [
    'https://cubbyclipboard.com/privacy',
  ]);
});

test('a Settings link is found whatever the constant is named or indented', () => {
  // SBS-1016: the old /^const \w*URL = / pattern missed these and
  // release:check still passed because the existing *URL constants kept
  // settingsOpenedUrls non-empty. extractHttpUrlLiterals catches all of them
  // by not caring about the binding at all.
  const source = [
    "const GITHUB_URL = 'https://github.com/btsouth/cubby-clipboard';",
    "  const DISCORD_LINK = 'https://discord.gg/cubby';",
    "export const SUPPORT_HREF = \"https://cubbyclipboard.com/support\";",
    "let DOCS_PAGE = 'https://cubbyclipboard.com/docs';",
  ].join('\n');
  assert.deepEqual(extractHttpUrlLiterals(source), [
    'https://github.com/btsouth/cubby-clipboard',
    'https://discord.gg/cubby',
    'https://cubbyclipboard.com/support',
    'https://cubbyclipboard.com/docs',
  ]);
});

test('extractOpenedUrls still finds an inline openUrl or handleOpenUrl string', () => {
  // handleOpenUrl is the Settings wrapper; openUrl is a case-sensitive
  // substring only of the plugin call, not of handleOpenUrl.
  assert.deepEqual(extractOpenedUrls("openUrl('https://cubbyclipboard.com/privacy')"), [
    'https://cubbyclipboard.com/privacy',
  ]);
  assert.deepEqual(extractOpenedUrls("handleOpenUrl('https://discord.gg/cubby')"), [
    'https://discord.gg/cubby',
  ]);
});

test('extractHttpUrlLiterals finds Settings object and call-site URLs the const pattern can miss', () => {
  const source = [
    "const LINKS = { discord: 'https://discord.gg/cubby' };",
    "const pages = ['https://cubbyclipboard.com/support'];",
    "content.startsWith('https://');",
  ].join('\n');
  assert.deepEqual(extractHttpUrlLiterals(source), [
    'https://discord.gg/cubby',
    'https://cubbyclipboard.com/support',
  ]);
});

test('live SettingsPanel quoted URLs are the three product links', async () => {
  const source = await readFile(
    path.join(repoRoot, 'frontend/src/components/SettingsPanel.tsx'),
    'utf8'
  );
  assert.deepEqual(extractHttpUrlLiterals(source), [
    'https://github.com/btsouth/cubby-clipboard',
    'https://cubbyclipboard.com',
    'https://cubbyclipboard.com/privacy',
  ]);
});

/**
 * SBS-1016 guard rail. extractOpenedUrls runs over every frontend/src .ts/.tsx,
 * so matching a bare `const X = 'https://…'` would sweep in the example.test
 * fixtures that live in *.test.ts and fail the release on URLs nothing opens.
 * Settings link constants are covered by extractHttpUrlLiterals instead.
 */
test('a bare const binding is not treated as an opened URL', () => {
  assert.deepEqual(extractOpenedUrls("  const href = 'https://example.test/a.png';"), []);
  assert.deepEqual(extractOpenedUrls("export const X = 'https://example.test/b.png';"), []);
  assert.deepEqual(extractOpenedUrls("  let u = 'https://example.test/c.png';"), []);
});

test('URLs handed straight to an open call are still collected', () => {
  assert.deepEqual(extractOpenedUrls("  openUrl('https://cubbyclipboard.com');"), [
    'https://cubbyclipboard.com',
  ]);
  assert.deepEqual(
    extractOpenedUrls("  void handleOpenUrl('https://cubbyclipboard.com/privacy');"),
    ['https://cubbyclipboard.com/privacy'],
  );
  // CRLF sources must not leave a trailing CR on the extracted URL.
  assert.deepEqual(extractOpenedUrls("  openUrl('https://a.example')\r"), ['https://a.example']);
});

/**
 * SBS-997 fidelity: glob documents that a `]` immediately after `[` or `[!` is
 * a literal specifier rather than the closer. Reading it as the closer makes
 * the class look empty and rejects the whole pattern, which is stricter than
 * the plugin enforces at runtime -- and a release gate that is stricter than
 * production is a gate that lies.
 */
test('a leading ] in a character class is a specifier, not the closer', () => {
  assert.equal(rustGlobMatches('https://example.com/[]]', 'https://example.com/]'), true);
  assert.equal(rustGlobMatches('https://example.com/[]]', 'https://example.com/x'), false);
  assert.equal(rustGlobMatches('https://example.com/[!]]', 'https://example.com/x'), true);
  assert.equal(rustGlobMatches('https://example.com/[!]]', 'https://example.com/]'), false);
});

test('ordinary and malformed character classes are unchanged', () => {
  assert.equal(rustGlobMatches('https://example.com/[abc]', 'https://example.com/b'), true);
  assert.equal(rustGlobMatches('https://example.com/[abc]', 'https://example.com/z'), false);
  assert.equal(rustGlobMatches('https://example.com/[!abc]', 'https://example.com/z'), true);
  // Genuinely empty and unclosed classes are still PatternError, so fail closed.
  assert.equal(rustGlobMatches('https://example.com/[]', 'https://example.com/'), false);
  assert.equal(rustGlobMatches('https://example.com/[abc', 'https://example.com/a'), false);
});
