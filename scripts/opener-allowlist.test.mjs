import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

import {
  assertAllowlistNotWideOpen,
  extractAllowlistPatterns,
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
  'https://github.com/tsouth89/cubby-clipboard',
];

const BLOCKED_URLS = [
  'https://evil.example',
  'https://github.com/other/repo',
  'https://cubbyclipboard.com.evil.com',
  'https://notcubbyclipboard.com/privacy',
];

const GITHUB_ONLY = ['https://github.com/tsouth89/cubby-clipboard'];

test('rust glob star does not cross a slash and question-mark is one char', () => {
  assert.equal(rustGlobMatches('https://example.com/*', 'https://example.com/privacy'), true);
  assert.equal(rustGlobMatches('https://example.com/*', 'https://example.com/'), true);
  assert.equal(rustGlobMatches('https://example.com/*', 'https://example.com'), false);
  assert.equal(rustGlobMatches('https://example.com/*', 'https://example.com/a/b'), false);
  assert.equal(rustGlobMatches('https://example.com', 'https://example.com'), true);
  assert.equal(rustGlobMatches('https://example.com', 'https://example.com/'), false);
  assert.equal(rustGlobMatches('https://ex?mple.com', 'https://example.com'), true);
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
  assert.equal(isUrlAllowed('https://github.com/tsouth89/cubby-clipboard', GITHUB_ONLY), true);
});

test('host-wildcard patterns are treated as opening the entire web', () => {
  assert.equal(isPatternWideOpen('https://*'), true);
  assert.equal(isPatternWideOpen('http://*'), true);
  assert.equal(isPatternWideOpen('*'), true);
  assert.equal(isPatternWideOpen('https://cubbyclipboard.com/*'), false);
  assert.throws(() => assertAllowlistNotWideOpen(['https://*']), /entire web/);
});

test('extractOpenedUrls finds Settings const URL assignments', () => {
  const source = [
    "const GITHUB_URL = 'https://github.com/tsouth89/cubby-clipboard';",
    "const WEBSITE_URL = 'https://cubbyclipboard.com';",
    "const PRIVACY_URL = 'https://cubbyclipboard.com/privacy';",
  ].join('\n');
  assert.deepEqual(extractOpenedUrls(source), [
    'https://github.com/tsouth89/cubby-clipboard',
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

