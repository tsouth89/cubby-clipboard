import { readFile, readdir } from 'node:fs/promises';
import path from 'node:path';

const root = path.resolve('product_pages');
const names = await readdir(root);
const files = new Set(names);
const pages = names.filter((name) => name.endsWith('.html'));
const errors = [];

for (const page of pages) {
  const html = await readFile(path.join(root, page), 'utf8');

  if (!/<title>[^<]+<\/title>/.test(html)) errors.push(`${page}: missing title`);
  if (!/<link rel="canonical" href="https:\/\/cubbyclipboard\.com\//.test(html)) {
    errors.push(`${page}: missing canonical cubbyclipboard.com URL`);
  }
  if (!html.includes('href="styles.css"')) errors.push(`${page}: missing shared stylesheet`);
  if (!html.includes('href="favicon.svg"')) errors.push(`${page}: missing favicon`);

  for (const [, href] of html.matchAll(/href="([^"]+)"/g)) {
    if (/^(https?:|mailto:|#)/.test(href)) continue;
    const target = href.split('#')[0];
    if (target && !files.has(target)) errors.push(`${page}: broken local link ${href}`);
  }
}

const activeSite = await Promise.all(
  names
    .filter((name) => /\.(?:html|css|svg)$/.test(name))
    .map((name) => readFile(path.join(root, name), 'utf8'))
);
const combined = activeSite.join('\n');

for (const forbidden of ['PastePaw', 'macOS', 'linear-gradient', 'purple', 'violet']) {
  if (combined.toLowerCase().includes(forbidden.toLowerCase())) {
    errors.push(`active site contains inherited or forbidden styling term: ${forbidden}`);
  }
}

if (pages.length !== 4) errors.push(`expected 4 HTML pages, found ${pages.length}`);
if (!files.has('_headers')) errors.push('missing Cloudflare Pages security headers');

if (errors.length) {
  console.error(errors.join('\n'));
  process.exit(1);
}

console.log(`Validated ${pages.length} Cubby website pages and their local links.`);
