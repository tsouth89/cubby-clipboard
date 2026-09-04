const ENDPOINT = "/api/events";
const REPOSITORY_URL = "https://github.com/btsouth/cubby-clipboard";

/**
 * True when `href` points at this project's GitHub repository.
 *
 * Deliberately not a bare `startsWith(REPOSITORY_URL)`: that also matches
 * sibling repositories whose names merely begin the same way, so a link to
 * `.../cubby-clipboard-docs` would be counted as a click on this repo. Require
 * the repository URL to end there or continue with a path, query, or fragment.
 */
export function isRepositoryUrl(href) {
  if (typeof href !== "string" || !href.startsWith(REPOSITORY_URL)) return false;
  const rest = href.slice(REPOSITORY_URL.length);
  return rest === "" || "/?#".includes(rest[0]);
}

/**
 * Decide which event a link click represents, or `null` for links we do not
 * track. Takes plain values rather than an element so it can be tested without
 * a DOM.
 *
 * Installer and release-page markers win over the repository check, because the
 * download buttons point at GitHub too and are the more specific intent.
 */
export function classifyLinkClick({ href, isLatestInstaller, isLatestRelease }) {
  if (isLatestInstaller) {
    return { event: "download_clicked", asset: "windows-x64-installer" };
  }
  if (isLatestRelease) {
    return { event: "download_clicked", asset: "release-page" };
  }
  if (isRepositoryUrl(href)) {
    return { event: "github_clicked" };
  }
  return null;
}

/**
 * Build the request body for one event.
 *
 * Takes the location object and reads `pathname` from it explicitly. The site
 * must never report query strings or fragments: those carry whatever a referrer
 * or campaign appended to the URL, which is exactly the kind of incidental
 * personal data the privacy policy promises not to collect.
 */
export function buildEventPayload(event, location, referrer, properties = {}) {
  return {
    event,
    pathname: location.pathname,
    referrer,
    ...properties,
  };
}

// Browser wiring. Guarded so importing this module in Node (the tests) does not
// try to touch document/location or fire a request.
if (typeof document !== "undefined") {
  const capture = (event, properties = {}) => {
    fetch(ENDPOINT, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(buildEventPayload(event, location, document.referrer, properties)),
      keepalive: true,
      credentials: "same-origin",
    }).catch(() => {
      // Analytics must never interfere with navigation or downloads.
    });
  };

  capture("$pageview");

  document.addEventListener("click", (event) => {
    if (!(event.target instanceof Element)) return;
    const link = event.target.closest("a[href]");
    if (!link) return;

    const classified = classifyLinkClick({
      href: link.href,
      isLatestInstaller: link.matches("[data-latest-installer]"),
      isLatestRelease: link.matches("[data-latest-release]"),
    });
    if (!classified) return;

    const { event: name, ...properties } = classified;
    if (name === "download_clicked") {
      properties.release =
        document.querySelector("[data-release-version]")?.textContent || "latest";
    }
    capture(name, properties);
  });
}
