import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readdir, readFile, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

import {
  PRIVILEGED_TRIGGERS,
  PRIVILEGED_WORKFLOWS,
  checkPrivilegedActionPins,
  classifyUses,
  parseWorkflowUses,
  privilegedTriggersIn,
} from './check-privileged-action-pins.mjs';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');

/**
 * SBS-984, generalized. This used to hardcode pr-review.yml, which meant
 * retiring that workflow left a stale PRIVILEGED_WORKFLOWS entry and broke
 * three CI steps with an ENOENT. Assert the invariant the filename stood for
 * instead: a `pull_request_target` workflow runs in the base-repo context with
 * write scopes, so every one of them must be pin-checked.
 */
test('every workflow with a privileged trigger is pin-checked', async () => {
  const workflowDir = path.join(repoRoot, '.github/workflows');
  const unlisted = [];
  for (const name of await readdir(workflowDir)) {
    if (!/\.ya?ml$/.test(name)) continue;
    const source = await readFile(path.join(workflowDir, name), 'utf8');
    const triggers = privilegedTriggersIn(source);
    if (triggers.length === 0) continue;
    const relativePath = `.github/workflows/${name}`;
    if (!PRIVILEGED_WORKFLOWS.includes(relativePath)) {
      unlisted.push(`${relativePath} (${triggers.join(', ')})`);
    }
  }
  assert.deepEqual(
    unlisted,
    [],
    `workflows with a privileged trigger missing from PRIVILEGED_WORKFLOWS (SBS-984): ${unlisted.join(', ')}`,
  );
});

/**
 * `on:` accepts a scalar, a flow sequence, a block sequence, and a mapping.
 * An earlier version of this guard matched only `pull_request_target:` at the
 * start of a line, so the other three shapes silently skipped pin checking.
 */
test('privileged triggers are detected in every on: shape', () => {
  const privileged = {
    mapping: 'on:\n  pull_request_target:\n    types: [opened]\njobs:\n',
    scalar: 'on: pull_request_target\njobs:\n',
    'flow sequence': 'on: [push, pull_request_target]\njobs:\n',
    'block sequence': 'on:\n  - push\n  - pull_request_target\njobs:\n',
    'quoted on key': '"on":\n  pull_request_target:\njobs:\n',
    workflow_run: 'on:\n  workflow_run:\n    workflows: [CI]\njobs:\n',
  };
  for (const [shape, source] of Object.entries(privileged)) {
    assert.ok(
      privilegedTriggersIn(source).length > 0,
      `${shape} should be recognized as privileged`,
    );
  }
});

test('a privileged trigger name outside the on: section does not count', () => {
  const benign = {
    'commented out': 'on:\n  push:  # not pull_request_target\njobs:\n',
    'referenced in a job condition':
      "on:\n  push:\njobs:\n  a:\n    if: github.event_name == 'pull_request_target'\n",
    'ordinary triggers': 'on:\n  push:\n    branches: [main]\njobs:\n',
    'no on section': 'jobs:\n  a:\n    steps: []\n',
  };
  for (const [shape, source] of Object.entries(benign)) {
    assert.deepEqual(privilegedTriggersIn(source), [], `${shape} should not be privileged`);
  }
});

test('privileged trigger list covers the base-repo-context triggers', () => {
  assert.ok(PRIVILEGED_TRIGGERS.includes('pull_request_target'));
  assert.ok(PRIVILEGED_TRIGGERS.includes('workflow_run'));
});

/** Every listed workflow must exist, and a stale entry must say so plainly. */
test('a listed workflow that no longer exists fails with an actionable message', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'sbs-984-'));
  await mkdir(path.join(root, '.github', 'workflows'), { recursive: true });
  await assert.rejects(
    () => checkPrivilegedActionPins(root),
    (error) => {
      assert.match(error.message, /listed in PRIVILEGED_WORKFLOWS but does not exist/);
      assert.match(error.message, /Drop the entry|update it/);
      return true;
    },
  );
});

test('every listed privileged workflow is present in the repo', async () => {
  for (const relativePath of PRIVILEGED_WORKFLOWS) {
    await assert.doesNotReject(
      () => readFile(path.join(repoRoot, relativePath), 'utf8'),
      `${relativePath} is listed in PRIVILEGED_WORKFLOWS but missing from the repo`,
    );
  }
});

test('SHA pin with a version comment is accepted', () => {
  const result = classifyUses(
    'azure/login@f5d393ae46f8fde4be8b75f32e3fc50e654ad0ca',
    '# v3.0.1',
  );
  assert.equal(result.ok, true);
  assert.equal(result.kind, 'pinned-third-party');
  assert.equal(result.version, 'v3.0.1');
});

test('mutable major and floating tags fail closed', () => {
  for (const spec of ['azure/login@v3', 'dtolnay/rust-toolchain@stable', 'taiki-e/install-action@v2.85.10']) {
    const result = classifyUses(spec, '');
    assert.equal(result.ok, false, spec);
    assert.equal(result.kind, 'mutable-third-party', spec);
  }
});

test('first-party actions may stay on a mutable tag', () => {
  const result = classifyUses('actions/checkout@v7', '');
  assert.equal(result.ok, true);
  assert.equal(result.kind, 'first-party');
});

test('local composite actions are allowed', () => {
  const result = classifyUses('./.github/actions/sign', '');
  assert.equal(result.ok, true);
  assert.equal(result.kind, 'local');
});

test('SHA pin without a version comment fails', () => {
  const result = classifyUses(
    'softprops/action-gh-release@3d0d9888cb7fd7b750713d6e236d1fcb99157228',
    '',
  );
  assert.equal(result.ok, false);
  assert.equal(result.kind, 'missing-version-comment');
});

test('uses line with a space before the colon is still parsed', () => {
  const findings = parseWorkflowUses('  uses : evil/action@v1\n', 'sample.yml');
  assert.equal(findings.length, 1);
  assert.equal(findings[0].spec, 'evil/action@v1');
  assert.equal(findings[0].ok, false);
  assert.equal(findings[0].kind, 'mutable-third-party');
});

test('uppercase 40-character SHA is accepted as a valid pin', () => {
  const result = classifyUses(`foo/bar@${'A'.repeat(40)}`, '# v1');
  assert.equal(result.ok, true);
  assert.equal(result.kind, 'pinned-third-party');
});

test('commented-out uses lines are ignored', () => {
  const findings = parseWorkflowUses(
    '# uses: evil/action@v1\n      - uses: actions/checkout@v7\n',
    'sample.yml',
  );
  assert.equal(findings.length, 1);
  assert.equal(findings[0].spec, 'actions/checkout@v7');
});

test('docker:// and missing @ref are unknown-fail, not empty-ok', () => {
  assert.equal(classifyUses('docker://alpine:3', '').ok, false);
  assert.equal(classifyUses('docker://alpine:3', '').kind, 'docker');
  assert.equal(classifyUses('owner/action', '').ok, false);
  assert.equal(classifyUses('owner/action', '').kind, 'unknown');
  assert.equal(classifyUses('', '').kind, 'unknown');
});

test('parser keeps the version comment that sits after the spec', () => {
  const findings = parseWorkflowUses(
    '        uses: microsoft/microsoft-store-apppublisher@cc9910a8d59f2eb55cbb83df0a3800cf3b5300e0 # v1.4\n',
    'sample.yml',
  );
  assert.equal(findings.length, 1);
  assert.equal(findings[0].ok, true);
  assert.equal(findings[0].version, 'v1.4');
});

test('checker fails a privileged workflow that still uses a mutable third-party tag', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'sbs-778-'));
  await mkdir(path.join(root, '.github', 'workflows'), { recursive: true });
  for (const relativePath of PRIVILEGED_WORKFLOWS) {
    const body =
      relativePath.endsWith('release.yml')
        ? 'jobs:\n  build:\n    steps:\n      - uses: azure/login@v3\n'
        : 'jobs:\n  publish:\n    steps:\n      - uses: actions/checkout@v7\n';
    await writeFile(path.join(root, relativePath), body);
  }
  const { violations } = await checkPrivilegedActionPins(root);
  assert.equal(violations.length, 1);
  assert.match(violations[0].reason, /not pinned to a 40-character commit SHA/);
  assert.equal(violations[0].spec, 'azure/login@v3');
});

/** Pins SBS-778: a retargeted @v3/@stable in release.yml must fail this suite. */
test('shipped privileged workflows have no mutable third-party uses', async () => {
  const { violations, pinnedThirdParty, findings } = await checkPrivilegedActionPins(repoRoot);
  assert.deepEqual(
    violations,
    [],
    violations.map((item) => `${item.file}:${item.line} ${item.reason}`).join('\n'),
  );
  assert.ok(findings.length > 0, 'expected to scan at least one uses: line');
  assert.ok(
    pinnedThirdParty.length > 0,
    'expected at least one SHA-pinned third-party action in privileged workflows',
  );
});

/** Pins SBS-778 wiring: the existing release:check gate must invoke the checker. */
test('release:check imports the privileged pin checker', async () => {
  const source = await readFile(path.join(repoRoot, 'scripts/check-release.mjs'), 'utf8');
  assert.match(source, /checkPrivilegedActionPins/);
});

/** Pins SBS-778: Dependabot must keep proposing reviewed github-actions pin updates. */
test('Dependabot is configured to propose GitHub Actions pin updates', async () => {
  const source = await readFile(path.join(repoRoot, '.github/dependabot.yml'), 'utf8');
  assert.match(source, /package-ecosystem:\s*github-actions/);
  assert.match(source, /RELEASE_CHECKLIST/);
});

/** Pins SBS-778: the review procedure for pin upgrades must stay in the checklist. */
test('release checklist documents the action-upgrade review procedure', async () => {
  const source = await readFile(path.join(repoRoot, 'docs/RELEASE_CHECKLIST.md'), 'utf8');
  assert.match(source, /SBS-778/);
  assert.match(source, /Do not retarget/);
});
