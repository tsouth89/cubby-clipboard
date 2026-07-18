(() => {
  const endpoint = "/api/events";

  const capture = (event, properties = {}) => {
    const payload = JSON.stringify({
      event,
      pathname: `${location.pathname}${location.search}`,
      referrer: document.referrer,
      ...properties,
    });

    fetch(endpoint, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: payload,
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

    const href = link.href;
    if (link.matches("[data-latest-installer], [data-latest-release]")) {
      capture("download_clicked", {
        asset: link.matches("[data-latest-installer]") ? "windows-x64-installer" : "release-page",
        release: document.querySelector("[data-release-version]")?.textContent || "latest",
      });
    } else if (href.startsWith("https://github.com/tsouth89/cubby-clipboard")) {
      capture("github_clicked");
    }
  });
})();
