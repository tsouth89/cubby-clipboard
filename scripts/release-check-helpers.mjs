// Shared helpers for scripts/check-release.mjs's SBS-811 secret-heuristics
// gate. Split out so they can be unit-tested without running the whole
// release-check script, which reads live repo files (and checks today's
// date) as soon as it's imported.

/**
 * Strip Rust line and block comments so a stale commented-out assignment
 * (e.g. `// skip_likely_secrets: false,`) can never be mistaken for the live
 * one.
 */
export function stripRustComments(source) {
  return source.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/[^\n]*/g, '');
}

/**
 * Read the shipped `skip_likely_secrets` default out of
 * `impl Default for AppSettings { fn default() -> Self { Self { ... } } }`.
 * Comments are stripped first, and the search is anchored to the `Self {`
 * struct literal inside that specific impl block (found by its true
 * top-level closing brace, i.e. a `}` at the very start of a line), so
 * neither a commented-out value above the real one nor an unrelated
 * `skip_likely_secrets: ...` occurrence elsewhere in the file can be picked
 * up instead.
 */
export function extractDefaultSkipLikelySecrets(modelsSource) {
  const stripped = stripRustComments(modelsSource);
  const implMatch = stripped.match(/impl Default for AppSettings \{[\s\S]*?\n\}/);
  if (!implMatch) return undefined;
  const selfIndex = implMatch[0].indexOf('Self {');
  if (selfIndex === -1) return undefined;
  const structLiteral = implMatch[0].slice(selfIndex);
  return structLiteral.match(/skip_likely_secrets:\s*(true|false)/)?.[1];
}

/**
 * Split a markdown document into logical bullet lines, joining a wrapped
 * bullet's continuation lines back into the bullet they belong to so a
 * phrase split across two physical lines is still checked as a whole.
 */
export function extractMarkdownBullets(markdown) {
  const bullets = [];
  for (const rawLine of markdown.split(/\r?\n/)) {
    const trimmed = rawLine.trim();
    if (/^-\s/.test(trimmed)) {
      bullets.push(trimmed);
      continue;
    }
    // Only wrapped continuations of the current dash item. Headings, later
    // paragraphs, and other list markers stay out of the previous bullet.
    const isContinuation =
      bullets.length > 0 &&
      /^\s+\S/.test(rawLine) &&
      !/^#{1,6}\s/.test(trimmed) &&
      !/^[-*+]\s/.test(trimmed) &&
      !/^\d+\.\s/.test(trimmed);
    if (isContinuation) {
      bullets[bullets.length - 1] += ` ${trimmed}`;
    }
  }
  return bullets;
}

// Word-boundary phrases, not preceded by "not ", so a sentence like
// "not opt-in" does not read as saying the default is off. "disabled by
// default" is accepted deliberately alongside "off by default" / "opt-in".
const DEFAULT_ON_PATTERN = /(?<!\bnot )\bdefault on\b/i;
export const DEFAULT_OFF_PATTERN = /(?<!\bnot )\b(off by default|opt-in|disabled by default)\b/i;

export function saysDefaultOff(text) {
  return DEFAULT_OFF_PATTERN.test(text);
}

export function saysDefaultOn(text) {
  return DEFAULT_ON_PATTERN.test(text);
}

/**
 * Find every bullet documenting the secret-heuristics gate and report
 * whether any of them says the default is on / off. Checking every matching
 * bullet (not just the first hit) means a second, unrelated mention of
 * "secret heuristics" cannot shadow the real bullet, and joining wrapped
 * lines first means a phrase split across two physical lines is still
 * detected.
 */
export function evaluateSecretHeuristicsDoc(securityDoc) {
  const bullets = extractMarkdownBullets(securityDoc).filter((bullet) =>
    /secret heuristics/i.test(bullet)
  );
  return {
    bullets,
    sayOn: bullets.some((bullet) => DEFAULT_ON_PATTERN.test(bullet)),
    sayOff: bullets.some((bullet) => DEFAULT_OFF_PATTERN.test(bullet)),
  };
}
