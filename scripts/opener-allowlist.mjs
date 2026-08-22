// SBS-810: opener allowlist matching used by check-release.mjs.

// tauri-plugin-opener compares the URL string the frontend passes to
// openUrl against each capability url with the Rust glob crate
// (Pattern::matches). Star and question-mark do not cross slash.
// This helper copies that rule so the release gate tests the same
// thing the plugin will enforce, not string equality of the JSON.

const GLOB_ESCAPE = /[\\^$+.()|[\]{}-]/g;

export function rustGlobMatches(pattern, value) {
  let regex = "^";
  for (const char of pattern) {
    if (char === "*") {
      regex += "[^/]*";
    } else if (char === "?") {
      regex += "[^/]";
    } else {
      regex += char.replace(GLOB_ESCAPE, "\\$&");
    }
  }
  regex += "$";
  return new RegExp(regex).test(value);
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
  const host = pattern.match(/^https?:\/\/([^/]*)/)?.[1];
  return host === "*" || host === "**" || host === "";
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

