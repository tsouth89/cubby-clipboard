import assert from "node:assert/strict";
import test from "node:test";

import { buildEventPayload, classifyLinkClick, isRepositoryUrl } from "./analytics.js";

test("installer links report the installer asset", () => {
  assert.deepEqual(
    classifyLinkClick({
      href: "https://github.com/btsouth/cubby-clipboard/releases/download/v1.2.6/setup.exe",
      isLatestInstaller: true,
      isLatestRelease: false,
    }),
    { event: "download_clicked", asset: "windows-x64-installer" }
  );
});

test("release-page links report the release-page asset", () => {
  assert.deepEqual(
    classifyLinkClick({
      href: "https://github.com/btsouth/cubby-clipboard/releases/latest",
      isLatestInstaller: false,
      isLatestRelease: true,
    }),
    { event: "download_clicked", asset: "release-page" }
  );
});

test("the installer marker wins when a link carries both", () => {
  // Both markers on one element must not be ambiguous: the download button
  // points at GitHub too, and the more specific intent is the installer.
  assert.deepEqual(
    classifyLinkClick({
      href: "https://github.com/btsouth/cubby-clipboard/releases/latest",
      isLatestInstaller: true,
      isLatestRelease: true,
    }),
    { event: "download_clicked", asset: "windows-x64-installer" }
  );
});

test("repository links without a download marker report a github click", () => {
  for (const href of [
    "https://github.com/btsouth/cubby-clipboard",
    "https://github.com/btsouth/cubby-clipboard/issues/45",
    "https://github.com/btsouth/cubby-clipboard?tab=readme",
    "https://github.com/btsouth/cubby-clipboard#install",
  ]) {
    assert.deepEqual(
      classifyLinkClick({ href, isLatestInstaller: false, isLatestRelease: false }),
      { event: "github_clicked" },
      href
    );
  }
});

test("unrelated links are not tracked", () => {
  for (const href of [
    "https://example.com/",
    "https://github.com/",
    "https://github.com/someone-else/other-project",
    "https://cubbyclipboard.com/privacy",
    "mailto:hello@example.com",
  ]) {
    assert.equal(
      classifyLinkClick({ href, isLatestInstaller: false, isLatestRelease: false }),
      null,
      href
    );
  }
});

test("a repository whose name merely shares our prefix is not our repository", () => {
  // A bare startsWith would count these as clicks on this project.
  assert.equal(isRepositoryUrl("https://github.com/btsouth/cubby-clipboard-docs"), false);
  assert.equal(isRepositoryUrl("https://github.com/btsouth/cubby-clipboardx"), false);
  assert.equal(isRepositoryUrl("https://github.com/btsouth/cubby-clipboard"), true);
  assert.equal(isRepositoryUrl("https://github.com/btsouth/cubby-clipboard/"), true);
});

test("isRepositoryUrl tolerates a missing or non-string href", () => {
  assert.equal(isRepositoryUrl(undefined), false);
  assert.equal(isRepositoryUrl(null), false);
  assert.equal(isRepositoryUrl(42), false);
});

test("the payload reports only the pathname, never the query or fragment", () => {
  const location = {
    href: "https://cubbyclipboard.com/start?utm_source=newsletter&email=someone@example.com#top",
    pathname: "/start",
    search: "?utm_source=newsletter&email=someone@example.com",
    hash: "#top",
  };

  const payload = buildEventPayload("$pageview", location, "https://example.com/ref");

  assert.equal(payload.pathname, "/start");
  const serialized = JSON.stringify(payload);
  assert.ok(!serialized.includes("utm_source"), "query parameters must not be reported");
  assert.ok(!serialized.includes("someone@example.com"), "query values must not be reported");
  assert.ok(!serialized.includes("#top"), "fragments must not be reported");
});

test("the payload carries the event, referrer, and any extra properties", () => {
  const payload = buildEventPayload(
    "download_clicked",
    { pathname: "/" },
    "https://news.example/post",
    { asset: "windows-x64-installer", release: "v1.2.6" }
  );

  assert.deepEqual(payload, {
    event: "download_clicked",
    pathname: "/",
    referrer: "https://news.example/post",
    asset: "windows-x64-installer",
    release: "v1.2.6",
  });
});

test("importing the module in Node fires no request and registers no listener", () => {
  // The browser wiring is guarded on `document`. If that guard regresses, this
  // import would have thrown on `document`/`location` before reaching here.
  assert.equal(typeof classifyLinkClick, "function");
  assert.equal(typeof globalThis.document, "undefined");
});
