import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

import { findFileListHistoryClaims } from './product-page-claims.mjs';

test('a positive support claim is reported', () => {
  const claims = findFileListHistoryClaims(
    '<p>It supports plain text, HTML, RTF, images, and file lists.</p>'
  );
  assert.equal(claims.length, 1);
  assert.match(claims[0], /file lists/);
});

test('a disclaimer is not reported', () => {
  assert.deepEqual(
    findFileListHistoryClaims('<p>We do not store file lists.</p>'),
    []
  );
});

test('other ways of disclaiming the support are not reported', () => {
  for (const sentence of [
    '<p>Copied file lists are no longer stored.</p>',
    '<p>Cubby ignores file-list clipboard events.</p>',
    '<p>Cubby cannot restore a file list from a backup.</p>',
    '<p>File lists are not part of history.</p>',
  ]) {
    assert.deepEqual(findFileListHistoryClaims(sentence), [], sentence);
  }
});

test('a negation about something else does not excuse the claim', () => {
  assert.equal(
    findFileListHistoryClaims('<p>It supports file lists, but not folders.</p>').length,
    1
  );
});

test('a claim next to an unrelated disclaimer is still reported', () => {
  const claims = findFileListHistoryClaims(
    '<ul><li>Cubby does not upload anything.</li><li>Cubby stores file lists.</li></ul>'
  );
  assert.deepEqual(claims, ['Cubby stores file lists.']);
});

test('a page with no mention passes', () => {
  assert.deepEqual(
    findFileListHistoryClaims('<p>It supports plain text, HTML, RTF, and images.</p>'),
    []
  );
});

test('an earlier negated clause does not hide a later file-list claim', () => {
  assert.equal(
    findFileListHistoryClaims('<p>Cubby does not upload anything and stores file lists.</p>').length,
    1
  );
  assert.equal(
    findFileListHistoryClaims('<p>Cubby never captures passwords, but it records file lists.</p>')
      .length,
    1
  );
});

test('inline tags and nbsp still count as a file-list mention', () => {
  assert.equal(
    findFileListHistoryClaims('<p>It supports file <strong>lists</strong>.</p>').length,
    1
  );
  assert.equal(findFileListHistoryClaims('<p>It supports file&nbsp;lists.</p>').length, 1);
});

test('copied-files wording is guarded the same way as file lists', () => {
  assert.equal(
    findFileListHistoryClaims('<p>Cubby records copied files in history.</p>').length,
    1
  );
  assert.deepEqual(
    findFileListHistoryClaims('<p>Copied files are not stored as history.</p>'),
    []
  );
});

test('file-list is not matched inside other words', () => {
  assert.deepEqual(
    findFileListHistoryClaims('<p>User profile lists are shown in Settings.</p>'),
    []
  );
});

test('a negation between the verb and the mention is a disclaimer', () => {
  assert.deepEqual(
    findFileListHistoryClaims('<p>Cubby stores text without file lists.</p>'),
    []
  );
});

test('gerund disclaimers are not reported', () => {
  assert.deepEqual(
    findFileListHistoryClaims('<p>Ignoring file lists since v1.2.4.</p>'),
    []
  );
});

// SBS-780: the claims that were on main at cf916e9. The weak sentence
// scan in check-release.mjs misses the README table cell (no verb). The
// helper missed "file-drop lists" until the mention pattern included it.

test('the original README table cell is a claim even without a verb', () => {
  const row =
    '| Text, HTML, RTF, images, and file lists | AES-256-GCM encryption for stored clipboard payloads |';
  const claims = findFileListHistoryClaims(row);
  assert.equal(claims.length, 1);
  assert.match(claims[0], /file lists/);
});

test('the original SECURITY.md file-drop sentence is a claim', () => {
  const sentence =
    'Core Windows clipboard representations are retained together: Unicode text, HTML, RTF, file-drop lists, and images.';
  const claims = findFileListHistoryClaims(sentence);
  assert.equal(claims.length, 1);
  assert.match(claims[0], /file-drop lists/);
});

test('a negated file-drop sentence is not a claim', () => {
  assert.deepEqual(
    findFileListHistoryClaims(
      'Cubby does not retain file-drop lists as clipboard history.'
    ),
    []
  );
});

test('live README.md and SECURITY.md do not claim file-list history', async () => {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
  for (const relative of ['README.md', 'SECURITY.md']) {
    const source = await readFile(path.join(root, relative), 'utf8');
    assert.deepEqual(findFileListHistoryClaims(source), [], relative);
  }
});
