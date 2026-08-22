import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

// Any workflow that runs in the base-repo context with write scopes belongs
// here, because an unpinned third-party action in one of these executes with
// those permissions (SBS-984). Every `pull_request_target` workflow qualifies;
// the test suite enforces that so a new one cannot be added and forgotten.
export const PRIVILEGED_WORKFLOWS = [
  '.github/workflows/release.yml',
  '.github/workflows/publish-store-packages.yml',
  '.github/workflows/validate-store-submission.yml',
];

// Triggers that run in the base-repo context with the repository's secrets and
// write scopes. An unpinned third-party action in a workflow with one of these
// is an escalation path, so such a workflow must be pin-checked (SBS-984).
export const PRIVILEGED_TRIGGERS = ['pull_request_target', 'workflow_run'];

/**
 * Return the workflow's `on:` section, comments stripped.
 *
 * GitHub accepts the trigger list as a scalar (`on: push`), a flow sequence
 * (`on: [push, pull_request_target]`), a block sequence, or a mapping. Matching
 * a single shape misses the others, so take the whole section and look for the
 * trigger token inside it. Scoping to the section also stops an unrelated
 * mention further down the file -- in an `if:` or a script step -- from
 * counting as a trigger.
 */
export function triggerSection(source) {
  const lines = source.split(/\r?\n/);
  const start = lines.findIndex((line) => /^(?:on|["']on["'])\s*:/.test(line));
  if (start === -1) return '';
  const section = [lines[start]];
  for (let index = start + 1; index < lines.length; index += 1) {
    // A new top-level key ends the section; blank and indented lines continue it.
    if (/^\S/.test(lines[index])) break;
    section.push(lines[index]);
  }
  return section.map((line) => line.replace(/#.*$/, '')).join('\n');
}

/** Which privileged triggers this workflow declares, if any. */
export function privilegedTriggersIn(source) {
  const section = triggerSection(source);
  return PRIVILEGED_TRIGGERS.filter((trigger) =>
    new RegExp(`\\b${trigger}\\b`).test(section),
  );
}

const FIRST_PARTY_OWNERS = new Set(['actions']);
const SHA_RE = /^[0-9a-f]{40}$/i;
const USES_RE = /^(?:-\s+)?uses\s*:\s*(?:['"]([^'"]+)['"]|(\S+))/;

/**
 * SBS-778: third-party actions in privileged workflows must be pinned to a
 * full commit SHA with a human-readable version comment. First-party
 * `actions/*` refs may stay mutable. Unknown / unparseable uses fail closed.
 */
export function classifyUses(rawSpec, comment) {
  const spec = rawSpec.trim();
  if (!spec) {
    return { ok: false, reason: 'empty uses value', kind: 'unknown' };
  }
  if (spec.startsWith('docker://')) {
    return { ok: false, reason: 'docker:// actions are not SHA-pinned commits', kind: 'docker' };
  }
  if (spec.startsWith('./')) {
    return { ok: true, kind: 'local', spec };
  }

  const at = spec.lastIndexOf('@');
  if (at <= 0) {
    return { ok: false, reason: 'uses is missing an @ref', kind: 'unknown', spec };
  }

  const action = spec.slice(0, at);
  const ref = spec.slice(at + 1);
  const owner = action.split('/')[0];

  if (!action.includes('/')) {
    return { ok: false, reason: 'unparseable action name', kind: 'unknown', spec };
  }

  if (FIRST_PARTY_OWNERS.has(owner)) {
    return { ok: true, kind: 'first-party', spec, action, ref };
  }

  if (!SHA_RE.test(ref)) {
    return {
      ok: false,
      reason: `third-party action is not pinned to a 40-character commit SHA (ref=${ref})`,
      kind: 'mutable-third-party',
      spec,
      action,
      ref,
    };
  }

  const version = (comment ?? '').replace(/^#\s*/, '').trim();
  if (!version) {
    return {
      ok: false,
      reason: 'SHA pin is missing an adjacent human-readable version comment',
      kind: 'missing-version-comment',
      spec,
      action,
      ref,
    };
  }

  return { ok: true, kind: 'pinned-third-party', spec, action, ref, version };
}

export function parseWorkflowUses(text, filePath = 'workflow.yml') {
  const findings = [];
  const lines = text.split(/\r?\n/);
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith('#')) {
      continue;
    }
    const match = trimmed.match(USES_RE);
    if (!match) {
      continue;
    }
    const spec = match[1] ?? match[2];
    const hash = line.indexOf('#');
    const specAt = line.indexOf(spec);
    const comment = hash >= 0 && hash > specAt ? line.slice(hash) : '';
    const classification = classifyUses(spec, comment);
    findings.push({
      file: filePath,
      line: index + 1,
      spec,
      comment,
      ...classification,
    });
  }
  return findings;
}

export function violationsOf(findings) {
  return findings.filter((finding) => !finding.ok);
}

export async function checkPrivilegedActionPins(rootDir) {
  const findings = [];
  for (const relativePath of PRIVILEGED_WORKFLOWS) {
    let text;
    try {
      text = await readFile(path.join(rootDir, relativePath), 'utf8');
    } catch (error) {
      if (error.code === 'ENOENT') {
        // A retired or renamed workflow must not surface as a bare ENOENT
        // stack trace three CI steps deep. Say which entry is stale and what
        // to do about it.
        throw new Error(
          `${relativePath} is listed in PRIVILEGED_WORKFLOWS but does not exist. ` +
            'Drop the entry if the workflow was retired, or update it if it was renamed.',
        );
      }
      throw error;
    }
    findings.push(...parseWorkflowUses(text, relativePath));
  }
  return {
    findings,
    violations: violationsOf(findings),
    pinnedThirdParty: findings.filter((finding) => finding.kind === 'pinned-third-party'),
  };
}

function repoRootFromHere() {
  return path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const root = process.argv[2] ? path.resolve(process.argv[2]) : repoRootFromHere();
  const { findings, violations, pinnedThirdParty } = await checkPrivilegedActionPins(root);
  if (violations.length > 0) {
    for (const violation of violations) {
      console.error(
        `${violation.file}:${violation.line}: ${violation.reason} (${violation.spec})`,
      );
    }
    console.error(
      `Privileged workflow pin check failed: ${violations.length} mutable or unknown third-party uses.`,
    );
    process.exit(1);
  }
  console.log(
    `Privileged workflow pin check passed: ${pinnedThirdParty.length} SHA-pinned third-party actions, ${findings.length} uses scanned.`,
  );
}
