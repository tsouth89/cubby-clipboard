import { readFile, readdir } from 'node:fs/promises';
import path from 'node:path';

const root = path.resolve('product_pages');
const names = await readdir(root);
const files = new Set(names);
const pages = names.filter((name) => name.endsWith('.html'));
const publicPages = new Map([
  ['index.html', 'https://cubbyclipboard.com/'],
  ['start.html', 'https://cubbyclipboard.com/start'],
  ['support.html', 'https://cubbyclipboard.com/support'],
  ['privacy.html', 'https://cubbyclipboard.com/privacy'],
  ['terms.html', 'https://cubbyclipboard.com/terms'],
]);
const cleanRoutes = new Map([
  ['/', 'index.html'],
  ['/start', 'start.html'],
  ['/support', 'support.html'],
  ['/privacy', 'privacy.html'],
  ['/terms', 'terms.html'],
]);
const errors = [];

for (const page of pages) {
  const html = await readFile(path.join(root, page), 'utf8');

  if (!/<title>[^<]+<\/title>/.test(html)) errors.push(`${page}: missing title`);
  if (!/<meta name="robots" content="[^"]+">/.test(html)) errors.push(`${page}: missing robots directive`);
  if (!html.includes('href="/styles.css"')) errors.push(`${page}: missing shared stylesheet`);
  if (!html.includes('href="/favicon-v2.svg"')) errors.push(`${page}: missing favicon`);

  const canonical = publicPages.get(page);
  if (canonical) {
    if (!html.includes('<meta name="description"')) errors.push(`${page}: missing description`);
    if (!html.includes(`rel="canonical" href="${canonical}"`)) errors.push(`${page}: canonical must use its clean production URL`);
    for (const required of [
      'property="og:type"',
      'property="og:url"',
      'property="og:site_name"',
      'property="og:title"',
      'property="og:description"',
      'property="og:image" content="https://cubbyclipboard.com/og-image.png"',
      'property="og:image:width" content="1200"',
      'property="og:image:height" content="630"',
      'name="twitter:card" content="summary_large_image"',
      'name="twitter:image" content="https://cubbyclipboard.com/og-image.png"',
      'rel="manifest" href="/site.webmanifest"',
    ]) {
      if (!html.includes(required)) errors.push(`${page}: missing ${required}`);
    }
  } else if (page === '404.html') {
    if (!html.includes('name="robots" content="noindex, follow"')) errors.push('404.html: must be noindex');
    if (html.includes('rel="canonical"')) errors.push('404.html: must not declare a canonical URL');
  } else {
    errors.push(`${page}: unexpected HTML page`);
  }

  for (const [, href] of html.matchAll(/href="([^"]+)"/g)) {
    if (/^(https?:|mailto:|#)/.test(href)) continue;
    const target = href.split(/[?#]/, 1)[0];
    if (!target) continue;
    if (target.startsWith('/')) {
      const routeFile = cleanRoutes.get(target);
      if (routeFile) continue;
      const asset = target.slice(1);
      if (files.has(asset)) continue;
    } else if (files.has(target)) {
      continue;
    }
    errors.push(`${page}: broken local link ${href}`);
  }
}

for (const required of ['_headers', '404.html', 'robots.txt', 'sitemap.xml', 'site.webmanifest', 'og-image.png', 'og-image.svg']) {
  if (!files.has(required)) errors.push(`missing website asset: ${required}`);
}

const sitemap = await readFile(path.join(root, 'sitemap.xml'), 'utf8');
const sitemapUrls = [...sitemap.matchAll(/<loc>([^<]+)<\/loc>/g)].map((match) => match[1]);
const canonicalUrls = [...publicPages.values()];
if (JSON.stringify(sitemapUrls) !== JSON.stringify(canonicalUrls)) {
  errors.push(`sitemap URLs differ from canonical pages: ${JSON.stringify(sitemapUrls)}`);
}

const robots = await readFile(path.join(root, 'robots.txt'), 'utf8');
if (!robots.includes('Sitemap: https://cubbyclipboard.com/sitemap.xml')) errors.push('robots.txt: missing sitemap URL');
if (!robots.includes('Disallow: /admin')) errors.push('robots.txt: private admin route must be excluded');

try {
  JSON.parse(await readFile(path.join(root, 'site.webmanifest'), 'utf8'));
} catch (error) {
  errors.push(`site.webmanifest: invalid JSON (${error.message})`);
}

const image = await readFile(path.join(root, 'og-image.png'));
const pngSignature = '89504e470d0a1a0a';
if (image.subarray(0, 8).toString('hex') !== pngSignature) errors.push('og-image.png: invalid PNG signature');
if (image.readUInt32BE(16) !== 1200 || image.readUInt32BE(20) !== 630) {
  errors.push(`og-image.png: expected 1200x630, got ${image.readUInt32BE(16)}x${image.readUInt32BE(20)}`);
}

const homepage = await readFile(path.join(root, 'index.html'), 'utf8');
const structuredData = homepage.match(/<script type="application\/ld\+json">([\s\S]+?)<\/script>/)?.[1];
try {
  const schema = JSON.parse(structuredData || '');
  if (schema['@type'] !== 'SoftwareApplication') errors.push('index.html: structured data must describe a SoftwareApplication');
} catch (error) {
  errors.push(`index.html: invalid JSON-LD (${error.message})`);
}

const activeSite = await Promise.all(
  names
    .filter((name) => /\.(?:html|css|svg)$/.test(name))
    .map((name) => readFile(path.join(root, name), 'utf8')),
);
const combined = activeSite.join('\n');

for (const forbidden of ['PastePaw', 'macOS', 'linear-gradient', 'purple', 'violet']) {
  if (combined.toLowerCase().includes(forbidden.toLowerCase())) {
    errors.push(`active site contains inherited or forbidden styling term: ${forbidden}`);
  }
}

// Em/en dashes read as AI-written. Use a regular hyphen, comma, or period.
if (combined.includes('—') || combined.includes('–')) {
  errors.push('active site contains an em or en dash (use a regular hyphen, comma, or period)');
}

if (pages.length !== 6) errors.push(`expected 6 HTML pages including 404.html, found ${pages.length}`);

if (errors.length) {
  console.error(errors.join('\n'));
  process.exit(1);
}

console.log(`Validated ${publicPages.size} indexable Cubby pages, 404 handling, social metadata, and crawl assets.`);
