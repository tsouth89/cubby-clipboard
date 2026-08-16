import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const CI_WORKFLOW = '.github/workflows/ci.yml';

/** SBS-779: every shipped Windows feature/arch pair must cargo-check in pre-merge CI. */
export const REQUIRED_LEGS = [
  { arch: 'x64', target: 'x86_64-pc-windows-msvc', features: 'default' },
  { arch: 'x64', target: 'x86_64-pc-windows-msvc', features: 'app-store' },
  { arch: 'arm64', target: 'aarch64-pc-windows-msvc', features: 'default' },
  { arch: 'arm64', target: 'aarch64-pc-windows-msvc', features: 'app-store' },
];

function stripQuote(value) {
  const trimmed = (value ?? '').trim();
  if (
    (trimmed.startsWith("'") && trimmed.endsWith("'")) ||
    (trimmed.startsWith('"') && trimmed.endsWith('"'))
  ) {
    return trimmed.slice(1, -1);
  }
  return trimmed;
}

function lineIndent(line) {
  const match = line.match(/^(\s*)/);
  return match ? match[1].length : 0;
}

/**
 * Split a workflow into top-level jobs. Unknown / unparseable YAML is an
 * empty list so the caller can fail closed instead of treating "no jobs"
 * as "all legs present".
 */
export function parseTopLevelJobs(text) {
  const lines = text.split(/\r?\n/);
  const jobsHeader = lines.findIndex((line) => line === 'jobs:');
  if (jobsHeader < 0) {
    return [];
  }

  const jobs = [];
  let current = null;
  for (let index = jobsHeader + 1; index < lines.length; index += 1) {
    const line = lines[index];
    const jobMatch = line.match(/^  ([A-Za-z0-9_-]+):\s*$/);
    if (jobMatch) {
      if (current) {
        jobs.push(current);
      }
      current = { id: jobMatch[1], startLine: index + 1, lines: [] };
      continue;
    }
    if (current) {
      current.lines.push(line);
    }
  }
  if (current) {
    jobs.push(current);
  }
  return jobs;
}

export function parseIncludeMaps(jobLines) {
  const includeIdx = jobLines.findIndex((line) => /^\s+include:\s*$/.test(line));
  if (includeIdx < 0) {
    return [];
  }
  const includeIndent = lineIndent(jobLines[includeIdx]);
  const items = [];
  let current = null;
  for (let index = includeIdx + 1; index < jobLines.length; index += 1) {
    const line = jobLines[index];
    if (!line.trim() || line.trim().startsWith('#')) {
      continue;
    }
    const indent = lineIndent(line);
    if (indent <= includeIndent) {
      break;
    }
    const itemStart = line.match(/^(\s+)-\s+([A-Za-z0-9_]+):\s*(.*)$/);
    if (itemStart && itemStart[1].length === includeIndent + 2) {
      if (current) {
        items.push(current);
      }
      current = { [itemStart[2]]: stripQuote(itemStart[3]) };
      continue;
    }
    const kv = line.match(/^\s+([A-Za-z0-9_]+):\s*(.*)$/);
    if (kv && current) {
      current[kv[1]] = stripQuote(kv[2]);
    }
  }
  if (current) {
    items.push(current);
  }
  return items;
}

function firstMatch(lines, pattern) {
  for (const line of lines) {
    if (line.trim().startsWith('#')) {
      continue;
    }
    const match = line.match(pattern);
    if (match) {
      return match[1] ?? match[0];
    }
  }
  return '';
}

function classifyLeg(item) {
  const arch = item.arch ?? '';
  const target = item.target ?? '';
  const features = item.features ?? '';
  const extraArgs = item.extra_args ?? '';
  const issues = [];

  if (!arch) {
    issues.push('matrix row is missing arch');
  }
  if (!target) {
    issues.push('matrix row is missing target');
  }
  if (features !== 'default' && features !== 'app-store') {
    issues.push(`matrix row features must be default or app-store (got ${features || 'empty'})`);
  }
  if (features === 'default' && extraArgs.includes('app-store')) {
    issues.push('default leg must not pass --features app-store');
  }
  if (features === 'app-store' && !extraArgs.includes('--features app-store')) {
    issues.push('app-store leg must pass --features app-store via extra_args');
  }

  return { arch, target, features, extraArgs, issues, ok: issues.length === 0 };
}

function jobProblems(job) {
  const body = job.lines.filter((line) => !line.trim().startsWith('#')).join('\n');
  const problems = [];
  const name = firstMatch(job.lines, /^\s+name:\s*(.+)$/);
  if (!/\$\{\{\s*matrix\.arch\s*\}\}/.test(name) || !/\$\{\{\s*matrix\.features\s*\}\}/.test(name)) {
    problems.push('job name must include matrix.arch and matrix.features');
  }
  if (!/runs-on:\s*windows-latest/.test(body)) {
    problems.push('job must run on windows-latest');
  }
  if (!/fail-fast:\s*false/.test(body)) {
    problems.push('matrix fail-fast must be false so one leg cannot hide another');
  }
  if (!/timeout-minutes:\s*\d+/.test(body)) {
    problems.push('job must set timeout-minutes so a wedged check cannot run for hours');
  }
  if (!/cargo\s+check\b/.test(body)) {
    problems.push('job must run cargo check');
  }
  if (/cargo\s+build\b/.test(body) || /tauri\s+build\b/.test(body)) {
    problems.push('pre-merge matrix must cargo check, not bundle');
  }
  if (!/--manifest-path\s+src-tauri\/Cargo\.toml/.test(body)) {
    problems.push('cargo check must use --manifest-path src-tauri/Cargo.toml');
  }
  if (!/--target\s+\$\{\{\s*matrix\.target\s*\}\}/.test(body)) {
    problems.push('cargo check must pass --target ${{ matrix.target }}');
  }
  if (!/--all-targets/.test(body)) {
    problems.push('cargo check must pass --all-targets so feature-gated bins and tests compile');
  }
  if (!/\$\{\{\s*matrix\.extra_args\s*\}\}/.test(body)) {
    problems.push('cargo check must apply matrix.extra_args so app-store is not a host-only flag');
  }
  const cacheKey = firstMatch(job.lines, /^\s+key:\s*(.+)$/);
  if (!/matrix\.target/.test(cacheKey) || !/matrix\.features/.test(cacheKey)) {
    problems.push('cache key must include matrix.target and matrix.features');
  }
  if (/dtolnay\/rust-toolchain/.test(body) && !/targets:\s*\$\{\{\s*matrix\.target\s*\}\}/.test(body)) {
    problems.push('rust-toolchain must install matrix.target');
  }
  return problems;
}

export function evaluateShippedWindowsCi(text) {
  const jobs = parseTopLevelJobs(text);
  const checkJobs = jobs.filter((job) => job.lines.some((line) => !line.trim().startsWith('#') && line.includes('cargo check')));
  const matched = [];
  const jobLevelProblems = [];

  if (checkJobs.length === 0) {
    return {
      ok: false,
      reason: 'no CI job runs cargo check',
      missing: REQUIRED_LEGS,
      matched,
      jobLevelProblems: ['no CI job runs cargo check'],
    };
  }

  for (const job of checkJobs) {
    const problems = jobProblems(job);
    if (problems.length > 0) {
      jobLevelProblems.push(...problems.map((problem) => `${job.id}: ${problem}`));
    }
    const includes = parseIncludeMaps(job.lines);
    if (includes.length === 0) {
      jobLevelProblems.push(`${job.id}: cargo check job has no matrix include rows`);
      continue;
    }
    for (const item of includes) {
      matched.push({ job: job.id, ...classifyLeg(item) });
    }
  }

  const missing = REQUIRED_LEGS.filter(
    (required) =>
      !matched.some(
        (leg) =>
          leg.ok &&
          leg.arch === required.arch &&
          leg.target === required.target &&
          leg.features === required.features,
      ),
  );
  const badLegs = matched.filter((leg) => !leg.ok);

  return {
    ok: missing.length === 0 && badLegs.length === 0 && jobLevelProblems.length === 0,
    missing,
    matched,
    badLegs,
    jobLevelProblems,
    reason:
      missing.length > 0
        ? `missing shipped legs: ${missing.map((leg) => `${leg.arch} ${leg.features}`).join(', ')}`
        : badLegs.length > 0
          ? `invalid matrix rows: ${badLegs.map((leg) => leg.issues.join('; ')).join(' | ')}`
          : jobLevelProblems[0] ?? '',
  };
}

export async function checkShippedWindowsCi(rootDir) {
  const text = await readFile(path.join(rootDir, CI_WORKFLOW), 'utf8');
  return evaluateShippedWindowsCi(text);
}

function repoRootFromHere() {
  return path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const root = process.argv[2] ? path.resolve(process.argv[2]) : repoRootFromHere();
  const result = await checkShippedWindowsCi(root);
  if (!result.ok) {
    for (const problem of result.jobLevelProblems) {
      console.error(problem);
    }
    for (const leg of result.badLegs) {
      console.error(`${leg.arch} ${leg.features}: ${leg.issues.join('; ')}`);
    }
    for (const leg of result.missing) {
      console.error(`missing ${leg.arch} ${leg.features} (${leg.target})`);
    }
    console.error(`Shipped Windows CI check failed: ${result.reason}`);
    process.exit(1);
  }
  console.log(
    `Shipped Windows CI check passed: ${result.matched.length} cargo-check legs cover x64/ARM64 default and app-store.`,
  );
}
