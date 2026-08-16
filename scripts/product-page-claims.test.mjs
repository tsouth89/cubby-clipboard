import assert from 'node:assert/strict';
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
