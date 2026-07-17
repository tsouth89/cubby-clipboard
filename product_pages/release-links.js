(() => {
  const releasesUrl = "https://github.com/tsouth89/cubby-clipboard/releases";
  const releasesApi =
    "https://api.github.com/repos/tsouth89/cubby-clipboard/releases?per_page=10";
  const cacheKey = "cubby-latest-release-v1";
  const cacheLifetimeMs = 60 * 60 * 1000;

  const isCubbyRelease = (release) =>
    release &&
    !release.draft &&
    typeof release.tag_name === "string" &&
    typeof release.html_url === "string" &&
    typeof release.published_at === "string" &&
    Number.isFinite(Date.parse(release.published_at)) &&
    release.html_url.startsWith(`${releasesUrl}/tag/`);

  const selectLatestRelease = (releases) =>
    releases
      .filter(isCubbyRelease)
      .reduce(
        (latest, release) =>
          !latest || Date.parse(release.published_at) > Date.parse(latest.published_at)
            ? release
            : latest,
        null,
      );

  const applyRelease = (release) => {
    if (!isCubbyRelease(release)) return;

    document.querySelectorAll("[data-latest-release]").forEach((link) => {
      link.href = release.html_url;
    });

    document.querySelectorAll("[data-release-version]").forEach((label) => {
      label.textContent = `${release.prerelease ? "Latest beta" : "Latest release"} · ${release.tag_name}`;
    });
  };

  const readCache = () => {
    try {
      const cached = JSON.parse(localStorage.getItem(cacheKey));
      if (Date.now() - cached.savedAt < cacheLifetimeMs && isCubbyRelease(cached.release)) {
        return cached.release;
      }
    } catch {
      // A blocked or malformed cache should never prevent the fallback link from working.
    }
    return null;
  };

  const cachedRelease = readCache();
  if (cachedRelease) {
    applyRelease(cachedRelease);
    return;
  }

  fetch(releasesApi, { headers: { Accept: "application/vnd.github+json" } })
    .then((response) => {
      if (!response.ok) throw new Error(`GitHub returned ${response.status}`);
      return response.json();
    })
    .then(selectLatestRelease)
    .then((release) => {
      if (!release) return;
      applyRelease(release);
      try {
        localStorage.setItem(cacheKey, JSON.stringify({ savedAt: Date.now(), release }));
      } catch {
        // The resolved link still works when storage is unavailable.
      }
    })
    .catch(() => {
      // Keep the releases-page fallback if GitHub is unavailable or rate-limited.
    });
})();
