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

// No dollar-anchor: sources can be CRLF, so a trailing CR would leave
// the pattern matching nothing and the gate passing on an empty set.
const OPENED_URL_PATTERN =
  /(?:^const \w*URL = |openUrl\(\s*)['"](https?:[^'"]+)['"]/gm;

export function extractOpenedUrls(source) {
  return [...source.matchAll(OPENED_URL_PATTERN)].map(([, url]) => url.replace(/\r$/, ""));
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

