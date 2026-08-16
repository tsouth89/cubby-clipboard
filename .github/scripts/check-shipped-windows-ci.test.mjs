import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises';
import { readFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

import {
  CI_WORKFLOW,
  REQUIRED_LEGS,
  checkShippedWindowsCi,
  evaluateShippedWindowsCi,
  parseIncludeMaps,
  parseTopLevelJobs,
} from './check-shipped-windows-ci.mjs';

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');

const VALID_JOB = `name: CI
jobs:
  test:
    runs-on: windows-latest
    steps:
      - run: cargo test --manifest-path src-tauri/Cargo.toml --all-targets
  check-shipped-windows:
    name: Check \${{ matrix.arch }} \${{ matrix.features }}
    runs-on: windows-latest
    timeout-minutes: 30
    strategy:
      fail-fast: false
      matrix:
        include:
          - arch: x64
            target: x86_64-pc-windows-msvc
            features: default
            extra_args: ''
          - arch: x64
            target: x86_64-pc-windows-msvc
            features: app-store
            extra_args: --features app-store
          - arch: arm64
            target: aarch64-pc-windows-msvc
            features: default
            extra_args: ''
          - arch: arm64
            target: aarch64-pc-windows-msvc
            features: app-store
            extra_args: --features app-store
    steps:
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: \${{ matrix.target }}
      - uses: actions/cache@v6
        with:
          key: \${{ runner.os }}-cargo-check-\${{ matrix.target }}-\${{ matrix.features }}-lock
      - run: mkdir dist
      - run: cargo check --manifest-path src-tauri/Cargo.toml --target \${{ matrix.target }} --all-targets \${{ matrix.extra_args }}
`;

test('parser splits top-level jobs and ignores deeper keys', () => {
  const jobs = parseTopLevelJobs(VALID_JOB);
  assert.deepEqual(
    jobs.map((job) => job.id),
    ['test', 'check-shipped-windows'],
  );
});

test('parser reads matrix include maps including empty extra_args', () => {
  const jobs = parseTopLevelJobs(VALID_JOB);
  const checkJob = jobs.find((job) => job.id === 'check-shipped-windows');
  const includes = parseIncludeMaps(checkJob.lines);
  assert.equal(includes.length, 4);
  assert.equal(includes[0].features, 'default');
  assert.equal(includes[0].extra_args, '');
  assert.equal(includes[1].extra_args, '--features app-store');
});

/** Pins SBS-779: host-only cargo test is not coverage of shipped Store/ARM64 configs. */
test('a workflow that only cargo-tests the host default is missing every shipped leg', () => {
  const result = evaluateShippedWindowsCi(`name: CI
jobs:
  test:
    runs-on: windows-latest
    steps:
      - run: cargo test --manifest-path src-tauri/Cargo.toml --all-targets
`);
  assert.equal(result.ok, false);
  assert.equal(result.missing.length, REQUIRED_LEGS.length);
  assert.match(result.reason, /no CI job runs cargo check/);
});

/** Pins SBS-779: dropping one architecture/feature pair must fail closed, not count as covered. */
test('a matrix missing ARM64 app-store is not complete', () => {
  const result = evaluateShippedWindowsCi(VALID_JOB.replace(
    `          - arch: arm64
            target: aarch64-pc-windows-msvc
            features: app-store
            extra_args: --features app-store
`,
    '',
  ));
  assert.equal(result.ok, false);
  assert.deepEqual(result.missing, [
    { arch: 'arm64', target: 'aarch64-pc-windows-msvc', features: 'app-store' },
  ]);
});

test('an app-store row that does not pass --features app-store is invalid, not a hit', () => {
  const result = evaluateShippedWindowsCi(VALID_JOB.replace(
    'extra_args: --features app-store',
    "extra_args: ''",
  ));
  assert.equal(result.ok, false);
  assert.ok(result.badLegs.some((leg) => leg.features === 'app-store'));
});

test('fail-fast true is rejected so one broken target cannot hide another', () => {
  const result = evaluateShippedWindowsCi(VALID_JOB.replace('fail-fast: false', 'fail-fast: true'));
  assert.equal(result.ok, false);
  assert.ok(result.jobLevelProblems.some((problem) => problem.includes('fail-fast')));
});

test('a cargo build / tauri build matrix is rejected as too expensive', () => {
  const result = evaluateShippedWindowsCi(
    VALID_JOB.replace('cargo check --manifest-path', 'cargo build --manifest-path'),
  );
  assert.equal(result.ok, false);
  assert.match(result.reason, /no CI job runs cargo check/);
});

test('a cargo check job that also cargo-builds is rejected as too expensive', () => {
  const result = evaluateShippedWindowsCi(
    VALID_JOB.replace(
      '- run: cargo check --manifest-path src-tauri/Cargo.toml --target ${{ matrix.target }} --all-targets ${{ matrix.extra_args }}',
      `- run: cargo check --manifest-path src-tauri/Cargo.toml --target \${{ matrix.target }} --all-targets \${{ matrix.extra_args }}
      - run: cargo build --manifest-path src-tauri/Cargo.toml --target \${{ matrix.target }}`,
    ),
  );
  assert.equal(result.ok, false);
  assert.ok(
    result.jobLevelProblems.some((problem) => problem.includes('not bundle')),
    result.jobLevelProblems.join('; '),
  );
});

test('an extra ubuntu cargo check job is not treated as the shipped-Windows matrix', () => {
  const result = evaluateShippedWindowsCi(`${VALID_JOB}
  lint:
    runs-on: ubuntu-latest
    steps:
      - run: cargo check --manifest-path src-tauri/Cargo.toml
`);
  assert.equal(result.ok, true, result.reason || result.jobLevelProblems.join('; '));
  assert.equal(result.matched.length, 4);
});

test('include rows indented more than two spaces still parse', () => {
  const result = evaluateShippedWindowsCi(
    VALID_JOB.replaceAll(
      '          - arch:',
      '            - arch:',
    ).replaceAll(
      '            target:',
      '              target:',
    ).replaceAll(
      '            features:',
      '              features:',
    ).replaceAll(
      '            extra_args:',
      '              extra_args:',
    ),
  );
  assert.equal(result.ok, true, result.reason || result.jobLevelProblems.join('; '));
  assert.equal(result.matched.length, 4);
});

test('a matrix that never creates dist is rejected', () => {
  const result = evaluateShippedWindowsCi(VALID_JOB.replace('      - run: mkdir dist\n', ''));
  assert.equal(result.ok, false);
  assert.ok(
    result.jobLevelProblems.some((problem) => problem.includes('create dist')),
    result.jobLevelProblems.join('; '),
  );
});

test('cargo check flags are not satisfied by a different cargo invocation in the same job', () => {
  const result = evaluateShippedWindowsCi(
    VALID_JOB.replace(
      '- run: cargo check --manifest-path src-tauri/Cargo.toml --target ${{ matrix.target }} --all-targets ${{ matrix.extra_args }}',
      `- run: cargo check --manifest-path src-tauri/Cargo.toml --target \${{ matrix.target }} \${{ matrix.extra_args }}
      - run: cargo test --all-targets`,
    ),
  );
  assert.equal(result.ok, false);
  assert.ok(
    result.jobLevelProblems.some((problem) => problem.includes('--all-targets')),
    result.jobLevelProblems.join('; '),
  );
});

test('a cache key that omits target is rejected so matrix legs cannot clobber each other', () => {
  const result = evaluateShippedWindowsCi(
    VALID_JOB.replace(
      'key: ${{ runner.os }}-cargo-check-${{ matrix.target }}-${{ matrix.features }}-lock',
      'key: ${{ runner.os }}-cargo-${{ hashFiles(\'src-tauri/Cargo.lock\') }}',
    ),
  );
  assert.equal(result.ok, false);
  assert.ok(result.jobLevelProblems.some((problem) => problem.includes('cache key')));
});

test('an unnamed matrix job is rejected so failures are not an opaque Check (matrix)', () => {
  const result = evaluateShippedWindowsCi(
    VALID_JOB.replace('name: Check ${{ matrix.arch }} ${{ matrix.features }}', 'name: Check Windows'),
  );
  assert.equal(result.ok, false);
  assert.ok(result.jobLevelProblems.some((problem) => problem.includes('job name')));
});

test('a valid four-leg cargo check matrix passes', () => {
  const result = evaluateShippedWindowsCi(VALID_JOB);
  assert.equal(result.ok, true, result.reason || result.jobLevelProblems.join('; '));
  assert.equal(result.missing.length, 0);
  assert.equal(result.matched.length, 4);
});

test('unparseable workflow fails closed instead of reporting full coverage', () => {
  const result = evaluateShippedWindowsCi('name: CI\n');
  assert.equal(result.ok, false);
  assert.equal(result.missing.length, REQUIRED_LEGS.length);
});

/** Pins SBS-779: shipped pre-merge CI must cargo-check all four Windows configs. */
test('shipped ci.yml cargo-checks x64/ARM64 default and app-store', async () => {
  const result = await checkShippedWindowsCi(repoRoot);
  assert.equal(
    result.ok,
    true,
    [result.reason, ...result.jobLevelProblems].filter(Boolean).join('\n'),
  );
  assert.equal(result.matched.length, REQUIRED_LEGS.length);
});

/** Pins SBS-779 wiring: the required CI job must invoke this checker. */
test('ci.yml runs the shipped-Windows checker and its tests', async () => {
  const source = await readFile(path.join(repoRoot, CI_WORKFLOW), 'utf8');
  assert.match(source, /check-shipped-windows-ci\.mjs/);
  assert.match(source, /check-shipped-windows-ci\.test\.mjs/);
});

test('checker fails a temp repo whose ci.yml still only tests the host default', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'sbs-779-'));
  await mkdir(path.join(root, '.github', 'workflows'), { recursive: true });
  await writeFile(
    path.join(root, CI_WORKFLOW),
    `name: CI
jobs:
  test:
    runs-on: windows-latest
    steps:
      - run: cargo test --manifest-path src-tauri/Cargo.toml --all-targets
`,
  );
  const result = await checkShippedWindowsCi(root);
  assert.equal(result.ok, false);
  assert.equal(result.missing.length, REQUIRED_LEGS.length);
});
