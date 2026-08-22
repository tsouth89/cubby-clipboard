// SBS-810: opener allowlist matching used by check-release.mjs.
// SBS-997: match glob::Pattern::matches defaults, and refuse host globs.

// tauri-plugin-opener compares the URL string the frontend passes to
// openUrl against each capability url with the Rust glob crate
// (Pattern::matches). That uses MatchOptions::new(), where
// require_literal_separator is false, so * and ? match `/`. Character
// classes are `[abc]` / `[!abc]`. This helper copies those defaults so
// the release gate tests the same thing the plugin will enforce, not
// string equality of the JSON and not a stricter-than-runtime model.

const GLOB_ESCAPE = /[\\^$+.()|{}]/g;
const GLOB_META = /[*?\[]/;

function compileCharClass(chars, start) {
  // glob::Pattern::new: '[' then optional '!' then at least one specifier
  // then ']'. Unclosed or empty classes are PatternError.
  if (start + 2 >= chars.length) return null;
  let i = start + 1;
  let negated = false;
  if (chars[i] === "!") {
    if (start + 3 >= chars.length) return null;
    negated = true;
    i += 1;
  }
  const bodyStart = i;
  // glob treats a `]` immediately after `[` or `[!` as a literal specifier
  // rather than the closer, so `[]]` matches `]` and `[!]]` matches not-`]`.
  // Searching from bodyStart would read those as an empty class and reject the
  // whole pattern, which is stricter than the plugin will be at runtime.
  const closeFrom = chars[bodyStart] === "]" ? bodyStart + 1 : bodyStart;
  const close = chars.indexOf("]", closeFrom);
  if (close === -1 || close === bodyStart) return null;
  const body = chars.slice(bodyStart, close);
  let cls = negated ? "[^" : "[";
  for (let k = 0; k < body.length; k += 1) {
    const c = body[k];
    if (c === "\\") {
      cls += "\\\\";
    } else if (c === "]") {
      // Only reachable for a leading `]`. Unescaped, JS would read `[]]` as an
      // empty class followed by a literal `]`, which matches nothing.
      cls += "\\]";
    } else if (c === "^" && k === 0 && !negated) {
      cls += "\\^";
    } else {
      cls += c;
    }
  }
  cls += "]";
  return { regex: cls, nextIndex: close + 1 };
}

function compileRustGlob(pattern) {
  let regex = "^";
  const chars = [...pattern];
  for (let i = 0; i < chars.length; ) {
    const char = chars[i];
    if (char === "*") {
      regex += ".*";
      i += 1;
    } else if (char === "?") {
      regex += ".";
      i += 1;
    } else if (char === "[") {
      const parsed = compileCharClass(chars, i);
      if (parsed === null) return null;
      regex += parsed.regex;
      i = parsed.nextIndex;
    } else {
      regex += char.replace(GLOB_ESCAPE, "\\$&");
      i += 1;
    }
  }
  regex += "$";
  try {
    return new RegExp(regex);
  } catch {
    return null;
  }
}

export function rustGlobMatches(pattern, value) {
  const compiled = compileRustGlob(pattern);
  if (compiled === null) return false;
  return compiled.test(value);
}

export function extractAllowlistPatterns(capability) {
  const openerScope = (capability?.permissions ?? []).find(
    (permission) => permission?.identifier === "opener:allow-open-url"
  );
  return (openerScope?.allow ?? [])
    .map((entry) => entry?.url)
    .filter((url) => typeof url === "string" && url.length > 0);
}

export function isUrlAllowed(url, patterns) {
  return patterns.some((pattern) => rustGlobMatches(pattern, url));
}

export function isPatternWideOpen(pattern) {
  if (pattern === "*" || pattern === "**") return true;
  const sep = pattern.indexOf("://");
  if (sep === -1) {
    // No scheme separator: a wildcard can match any URL, including hosts.
    return GLOB_META.test(pattern);
  }
  const scheme = pattern.slice(0, sep);
  const rest = pattern.slice(sep + 3);
  if (scheme === "" || GLOB_META.test(scheme)) return true;
  const slash = rest.indexOf("/");
  const authority = slash === -1 ? rest : rest.slice(0, slash);
  if (authority === "") return true;
  return GLOB_META.test(authority);
}

export function assertAllowlistNotWideOpen(patterns) {
  const wide = patterns.filter(isPatternWideOpen);
  if (wide.length > 0) {
    throw new Error(
      "Opener allowlist must not permit the entire web; refused: " + wide.join(", ")
    );
  }
}

// SBS-1016: a URL literal handed straight to openUrl( / handleOpenUrl(.
// This runs over all of frontend/src, so it must only match something that
// is demonstrably opened. It deliberately does NOT match a bare
// `const X = 'https://…'` binding: that would sweep in the example.test
// fixtures in *.test.ts and fail the release on URLs nothing ever opens,
// which is the same reason extractHttpUrlLiterals below is kept off
// frontend/src at large. `const DISCORD_LINK = 'https://…'` in Settings is
// covered by that literal scan instead, which is where SBS-1016's widening
// belongs -- Settings is the only surface that opens URLs.
// No dollar-anchor: sources can be CRLF, so a trailing CR would leave
// the pattern matching nothing and the gate passing on an empty set.
const OPENED_URL_PATTERN = /(?:handleO|o)penUrl\(\s*['"](https?:\/\/[^'"]+)['"]/g;

// Quoted http(s) URL with a host. Prefix-only checks such as 'https://'
// do not match because [^'"]+ requires at least one character after ://.
const HTTP_URL_LITERAL = /['"](https?:\/\/[^'"]+)['"]/g;

function stripTrailingCr(url) {
  return url.replace(/\r$/, "");
}

export function extractOpenedUrls(source) {
  return [...source.matchAll(OPENED_URL_PATTERN)].map(([, url]) => stripTrailingCr(url));
}

// Settings is the only surface that opens URLs today. Any quoted http(s)
// URL there is treated as a link that must be on the allowlist. Not used
// on the rest of frontend/src, where test fixtures contain example.test
// URLs that are never opened.
export function extractHttpUrlLiterals(source) {
  return [...source.matchAll(HTTP_URL_LITERAL)].map(([, url]) => stripTrailingCr(url));
}

export function urlSlashVariants(url) {
  // rust-url adds a trailing slash only to origin URLs. Path URLs such as
  // /privacy stay as written, and the live privacy canonical has no slash.
  try {
    const parsed = new URL(url);
    const originOnly = parsed.pathname === "/" || parsed.pathname === "";
    if (!originOnly) return [url];
    const origin = parsed.protocol + "//" + parsed.host;
    return [origin, origin + "/"];
  } catch {
    return [url];
  }
}
