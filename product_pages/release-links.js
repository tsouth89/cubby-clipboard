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

  const findInstaller = (release) => {
    if (!Array.isArray(release.assets)) return null;
    const asset = release.assets.find(
      (a) =>
        a &&
        typeof a.name === "string" &&
        typeof a.browser_download_url === "string" &&
        /x64-setup\.exe$/i.test(a.name),
    );
    return asset ? asset.browser_download_url : null;
  };

  const applyRelease = (release) => {
    if (!isCubbyRelease(release)) return;

    document.querySelectorAll("[data-latest-release]").forEach((link) => {
      link.href = release.html_url;
    });

    // Direct one-click installer download for non-technical visitors, so they
    // never have to pick a file off the GitHub releases page. Falls back to the
    // releases page (the element's existing href) when no installer is found.
    const installerUrl = findInstaller(release);
    if (installerUrl) {
      document.querySelectorAll("[data-latest-installer]").forEach((link) => {
        link.href = installerUrl;
      });
    }

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

  const resolveRelease = () => {
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
  };

  // The star count only renders once it is social proof rather than an
  // admission of obscurity; below the threshold the button stays a plain CTA.
  const repoApi = "https://api.github.com/repos/tsouth89/cubby-clipboard";
  const starCacheKey = "cubby-star-count-v1";
  const minStarsToShow = 75;

  const applyStars = (count) => {
    if (!Number.isFinite(count) || count < minStarsToShow) return;
    const formatted =
      count >= 1000 ? `${(count / 1000).toFixed(1).replace(/\.0$/, "")}k` : `${count}`;
    document.querySelectorAll("[data-star-count]").forEach((el) => {
      el.textContent = formatted;
      el.hidden = false;
    });
  };

  const resolveStars = () => {
    try {
      const cached = JSON.parse(localStorage.getItem(starCacheKey));
      if (Date.now() - cached.savedAt < cacheLifetimeMs && Number.isFinite(cached.count)) {
        applyStars(cached.count);
        return;
      }
    } catch {
      // Fall through to a fresh fetch.
    }

    fetch(repoApi, { headers: { Accept: "application/vnd.github+json" } })
      .then((response) => {
        if (!response.ok) throw new Error(`GitHub returned ${response.status}`);
        return response.json();
      })
      .then((repo) => {
        const count = repo && repo.stargazers_count;
        if (!Number.isFinite(count)) return;
        applyStars(count);
        try {
          localStorage.setItem(starCacheKey, JSON.stringify({ savedAt: Date.now(), count }));
        } catch {
          // The button works fine without a count.
        }
      })
      .catch(() => {
        // The button works fine without a count.
      });
  };

  resolveRelease();
  resolveStars();
})();
