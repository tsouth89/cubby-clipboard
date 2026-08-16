import assert from 'node:assert/strict';
import test from 'node:test';

import {
  evaluateSecretHeuristicsDoc,
  extractDefaultSkipLikelySecrets,
} from './release-check-helpers.mjs';

test('reads the live default when a stale commented-out value sits above it', () => {
  const modelsSource = `
impl Default for AppSettings {
    fn default() -> Self {
        Self {
            // skip_likely_secrets: false,
            skip_likely_secrets: true,
        }
    }
}
`;
  assert.equal(extractDefaultSkipLikelySecrets(modelsSource), 'true');
});

test('ignores a skip_likely_secrets occurrence outside the AppSettings default impl', () => {
  const modelsSource = `
// A test helper that also mentions skip_likely_secrets: true, but is not
// the shipped default.
fn make_test_settings() -> AppSettings {
    AppSettings { skip_likely_secrets: true, ..Default::default() }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            skip_likely_secrets: false,
        }
    }
}
`;
  assert.equal(extractDefaultSkipLikelySecrets(modelsSource), 'false');
});

test('returns undefined when there is no AppSettings default impl at all', () => {
  const modelsSource = 'pub struct AppSettings { pub skip_likely_secrets: bool }';
  assert.equal(extractDefaultSkipLikelySecrets(modelsSource), undefined);
});

test('detects a secret-heuristics bullet wrapped across two lines', () => {
  const securityDoc = `
- Text that matches high-confidence secret heuristics such as private keys
  and cloud API tokens (off by default, enable in Settings).
`;
  const { bullets, sayOff, sayOn } = evaluateSecretHeuristicsDoc(securityDoc);
  assert.equal(bullets.length, 1);
  assert.equal(sayOff, true);
  assert.equal(sayOn, false);
});

test('checks every bullet mentioning secret heuristics, not just the first', () => {
  const securityDoc = `
- Secret heuristics are mentioned here in an unrelated aside with no on/off phrase.
- The real bullet: secret heuristics are off by default, opt-in in Settings.
`;
  const { sayOff } = evaluateSecretHeuristicsDoc(securityDoc);
  assert.equal(sayOff, true);
});

test("does not read 'not opt-in' as saying the default is off", () => {
  const securityDoc = '- Our secret heuristics are not opt-in; they run automatically.';
  const { sayOff } = evaluateSecretHeuristicsDoc(securityDoc);
  assert.equal(sayOff, false);
});

test("accepts 'disabled by default' as a deliberate off-by-default phrasing", () => {
  const securityDoc = '- Secret heuristics scanning is disabled by default.';
  const { sayOff } = evaluateSecretHeuristicsDoc(securityDoc);
  assert.equal(sayOff, true);
});
