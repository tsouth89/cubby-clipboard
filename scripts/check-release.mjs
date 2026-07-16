import { readFile } from 'node:fs/promises';

const root = new URL('../', import.meta.url);
const read = (path) => readFile(new URL(path, root), 'utf8');

const [packageText, tauriText, cargoText, changelog, releaseWorkflow] = await Promise.all([
  read('package.json'),
  read('src-tauri/tauri.conf.json'),
  read('src-tauri/Cargo.toml'),
  read('CHANGELOG.md'),
  read('.github/workflows/release.yml'),
]);

const packageVersion = JSON.parse(packageText).version;
const tauriConfig = JSON.parse(tauriText);
const cargoVersion = cargoText.match(/^version = "([^"]+)"/m)?.[1];
const versions = new Map([
  ['package.json', packageVersion],
  ['src-tauri/tauri.conf.json', tauriConfig.version],
  ['src-tauri/Cargo.toml', cargoVersion],
]);
const uniqueVersions = new Set(versions.values());

if (uniqueVersions.size !== 1 || uniqueVersions.has(undefined)) {
  throw new Error(
    `Release versions do not match: ${[...versions].map(([file, version]) => `${file}=${version ?? 'missing'}`).join(', ')}`
  );
}

const version = packageVersion;
if (!changelog.includes(`\n## v${version}\n`)) {
  throw new Error(`CHANGELOG.md has no v${version} section`);
}

if (JSON.stringify(tauriConfig.bundle.targets) !== JSON.stringify(['nsis'])) {
  throw new Error('Release bundles must be limited to the Windows NSIS installer');
}

for (const inheritedIdentity of ['PastePaw', 'XueshiQiao.PastePaw', 'XueshiQiao.github.io']) {
  if (releaseWorkflow.includes(inheritedIdentity)) {
    throw new Error(`Release workflow still contains inherited identity: ${inheritedIdentity}`);
  }
}

console.log(`Cubby Clipboard v${version} release metadata is consistent.`);
