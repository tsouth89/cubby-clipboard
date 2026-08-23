import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

import {
  claimSegments,
  findFileListHistoryClaims,
  findStaleBackupOmissionClaims,
  findUnqualifiedRemoteHotkeyClaims,
  findWeakerFileRetentionClaim,
} from './product-page-claims.mjs';

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

// The mention needs "list" after "drop". A bare "file drop" was both too wide
// and self-cancelling: NEGATION contains \bdrops\b, so the plural negated its
// own no-verb cell while the singular reported a false claim.

test('a bare file-drop mention is not a claim', () => {
  assert.deepEqual(
    findFileListHistoryClaims(
      'Windows file-drop (CF_HDROP) is a path list, not clipboard content.'
    ),
    []
  );
  assert.deepEqual(findFileListHistoryClaims('<p>Use the file drop-down.</p>'), []);
});

test('a no-verb cell naming file-drop lists is still a claim', () => {
  const row = '| Text, HTML, RTF, images, and file-drop lists | Encrypted at rest |';
  const claims = findFileListHistoryClaims(row);
  assert.equal(claims.length, 1);
  assert.match(claims[0], /file-drop lists/);
});

// claimSegments is what the weaker \bfiles?\b scan in check-release.mjs uses.
// A [.!?] split alone does not break at `.</p>` or `disk.",`, which joined a
// whole install-steps section into one span.

test('claimSegments breaks at markup and JSON boundaries', () => {
  const page =
    '<p>A file will save to your computer.</p>\n<p>This includes a SmartScreen warning.</p>';
  const found = claimSegments(page).find(
    (segment) => /\bfiles?\b/i.test(segment) && /\bincludes?\b/i.test(segment)
  );
  assert.equal(found, undefined, 'the install steps must not read as one sentence');

  const locale = '{\n  "a": "Leftover files stay on disk.",\n  "b": "This includes the log."\n}';
  const joined = claimSegments(locale).find(
    (segment) => /\bfiles?\b/i.test(segment) && /\bincludes?\b/i.test(segment)
  );
  assert.equal(joined, undefined, 'two locale strings must not read as one sentence');
});

test('weaker file-retention scan treats no/without as negation', () => {
  assert.equal(findWeakerFileRetentionClaim('No files are retained.'), undefined);
  assert.equal(findWeakerFileRetentionClaim('Without storing files.'), undefined);
  assert.match(findWeakerFileRetentionClaim('Cubby stores files.') ?? '', /stores files/);
});

// SBS-1028: privacy.html said encrypted backups omit HTML/RTF and
// full-resolution originals after export_backup started including them.

test('the original privacy.html backup omission is a claim', () => {
  const sentence =
    'It does not hold the HTML and RTF copies of a clip or the full-resolution image files, so those do not come back on import.';
  const claims = findStaleBackupOmissionClaims(sentence);
  assert.equal(claims.length, 1);
  assert.match(claims[0], /does not hold the HTML and RTF/);
});

test('a restated backup omission is a claim', () => {
  assert.equal(
    findStaleBackupOmissionClaims(
      'The archive does not include HTML and RTF copies of a clip.'
    ).length,
    1
  );
  assert.equal(
    findStaleBackupOmissionClaims(
      'Encrypted backups do not contain the full-resolution originals.'
    ).length,
    1
  );
});

test('accurate backup copy is not reported', () => {
  for (const sentence of [
    'The archive holds each clip’s text, HTML and RTF copies, and any live full-resolution original.',
    'Export refuses to write the file if a live original or format cannot be read.',
    'An image whose full-resolution original has already been dropped by retention stays thumbnail-only.',
    'Cubby does not store the passphrase. The archive holds HTML and RTF copies.',
  ]) {
    assert.deepEqual(findStaleBackupOmissionClaims(sentence), [], sentence);
  }
});

test('an unrelated does-not-include sentence is not reported', () => {
  assert.deepEqual(
    findStaleBackupOmissionClaims(
      'Update checks do not include clipboard content or product-usage events.'
    ),
    []
  );
});

test('omit-verb restatements about the archive are claims', () => {
  assert.equal(
    findStaleBackupOmissionClaims('The archive omits HTML and RTF copies').length,
    1
  );
  assert.equal(
    findStaleBackupOmissionClaims('The archive omits HTML and RTF copies of a clip.').length,
    1
  );
  assert.equal(
    findStaleBackupOmissionClaims(
      'Encrypted backups omitted the full-resolution originals.'
    ).length,
    1
  );
});

test('privacy denials that are not about the archive are not claims', () => {
  assert.deepEqual(
    findStaleBackupOmissionClaims('Cubby does not store HTML and RTF on our servers'),
    []
  );
  assert.deepEqual(
    findStaleBackupOmissionClaims('Cubby does not include HTML and RTF in telemetry.'),
    []
  );
});

test('expired originals that do not come back are not a backup claim', () => {
  assert.deepEqual(
    findStaleBackupOmissionClaims(
      'Full-resolution originals do not come back after retention drops them.'
    ),
    []
  );
});

test('a prior export sentence does not flag retention come-back wording', () => {
  assert.deepEqual(
    findStaleBackupOmissionClaims(
      'Export writes a local encrypted file. Full-resolution originals do not come back after retention drops them.'
    ),
    []
  );
});

test('anaphoric archive omission is a claim', () => {
  const source =
    'The archive is a local encrypted file. It does not hold the HTML and RTF copies of a clip or the full-resolution image files.';
  const claims = findStaleBackupOmissionClaims(source);
  assert.equal(claims.length, 1);
  assert.match(claims[0], /does not hold the HTML and RTF/);
});

test('negated omit verbs are not claims', () => {
  assert.deepEqual(
    findStaleBackupOmissionClaims('The archive does not omit HTML and RTF copies.'),
    []
  );
  assert.deepEqual(
    findStaleBackupOmissionClaims(
      'Encrypted backups never omit the full-resolution originals.'
    ),
    []
  );
});

test('export restatements of the omission are claims', () => {
  assert.equal(
    findStaleBackupOmissionClaims('The export omits HTML and RTF copies of a clip.')
      .length,
    1
  );
  assert.equal(
    findStaleBackupOmissionClaims(
      'Exported archives do not include the full-resolution originals.'
    ).length,
    1
  );
});

test('anaphoric export omission is a claim', () => {
  const source =
    'Export writes a local encrypted file. It does not hold the HTML and RTF copies of a clip or the full-resolution image files.';
  const claims = findStaleBackupOmissionClaims(source);
  assert.equal(claims.length, 1);
  assert.match(claims[0], /does not hold the HTML and RTF/);
});

test('live privacy.html does not claim backups omit formats', async () => {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
  const source = await readFile(path.join(root, 'product_pages/privacy.html'), 'utf8');
  assert.deepEqual(findStaleBackupOmissionClaims(source), []);
});

// SBS-1049: Settings and support said the configured hotkey opens Cubby
// in remote sessions whenever keyboard forwarding is off. That path is
// the Win+V helper hook, so replacement must be named.

test('the original Settings remote-hotkey hint is a claim', () => {
  const sentence =
    'Your hotkey opens Cubby inside remote sessions when the remote client\'s keyboard forwarding is off.';
  const claims = findUnqualifiedRemoteHotkeyClaims(sentence);
  assert.equal(claims.length, 1);
  assert.match(claims[0], /hotkey opens Cubby/);
});

test('the original support FAQ trigger sentence is a claim', () => {
  const sentence =
    'A remote-session trigger is also available for workflows where the remote client intercepts Windows shortcuts.';
  const claims = findUnqualifiedRemoteHotkeyClaims(sentence);
  assert.equal(claims.length, 1);
  assert.match(claims[0], /remote-session trigger/);
});

test('a restated remote-hotkey claim without replacement is reported', () => {
  assert.equal(
    findUnqualifiedRemoteHotkeyClaims(
      'Your configured hotkey can open Cubby inside a remote session if keyboard forwarding is off.'
    ).length,
    1
  );
});

test('naming Win+V replacement qualifies the remote-hotkey sentence', () => {
  for (const sentence of [
    'When Replace Windows clipboard shortcut is on, your hotkey opens Cubby inside remote sessions if keyboard forwarding is off.',
    'Win+V replacement also lets your hotkey open Cubby inside remote sessions.',
    'The helper hook opens Cubby in a remote session when forwarding is off.',
    'The trigger is available in a remote session only while replacement mode is enabled.',
  ]) {
    assert.deepEqual(findUnqualifiedRemoteHotkeyClaims(sentence), [], sentence);
  }
});

test('remote paste and tray fallback copy are not remote-hotkey claims', () => {
  for (const sentence of [
    'With forwarding on, open Cubby from the tray icon.',
    'RDP and third-party remote tools decide whether Windows shortcuts stay local.',
    'Cubby includes a remote-session workflow for this reason.',
    'Cubby is designed to use Win+V as its primary shortcut.',
  ]) {
    assert.deepEqual(findUnqualifiedRemoteHotkeyClaims(sentence), [], sentence);
  }
});

test('live Settings and support copy name the Win+V replacement dependency', async () => {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
  for (const relative of [
    'frontend/src/i18n/locales/en.json',
    'product_pages/support.html',
  ]) {
    const source = await readFile(path.join(root, relative), 'utf8');
    assert.deepEqual(findUnqualifiedRemoteHotkeyClaims(source), [], relative);
  }
});
