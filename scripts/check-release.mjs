import { readFile } from 'node:fs/promises';

const root = new URL('../', import.meta.url);
const read = (path) => readFile(new URL(path, root), 'utf8');

const [packageText, tauriText, cargoText, changelog, releaseWorkflow, capabilityText, clipboardSource, cryptoSource, databaseSource, commandSource, clipCardSource] = await Promise.all([
  read('package.json'),
  read('src-tauri/tauri.conf.json'),
  read('src-tauri/Cargo.toml'),
  read('CHANGELOG.md'),
  read('.github/workflows/release.yml'),
  read('src-tauri/capabilities/default.json'),
  read('src-tauri/src/clipboard.rs'),
  read('src-tauri/src/crypto.rs'),
  read('src-tauri/src/database.rs'),
  read('src-tauri/src/commands.rs'),
  read('frontend/src/components/ClipCard.tsx'),
]);

const packageVersion = JSON.parse(packageText).version;
const tauriConfig = JSON.parse(tauriText);
const capability = JSON.parse(capabilityText);
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
const changelogHeading = new RegExp(`^## v${version.replaceAll('.', '\\.')}$`, 'm');
if (!changelogHeading.test(changelog)) {
  throw new Error(`CHANGELOG.md has no v${version} section`);
}

if (JSON.stringify(tauriConfig.bundle.targets) !== JSON.stringify(['nsis'])) {
  throw new Error('Release bundles must be limited to the Windows NSIS installer');
}

const csp = tauriConfig.app?.security?.csp;
if (typeof csp !== 'string' || !csp.includes("default-src 'self'") || !csp.includes("object-src 'none'")) {
  throw new Error('Release builds must use the restrictive Cubby content-security policy');
}

if (JSON.stringify(capability.windows) !== JSON.stringify(['main', 'settings'])) {
  throw new Error('Tauri capabilities must be scoped to the main and settings windows');
}

for (const forbiddenPermission of ['notification:default', 'opener:default', 'clipboard-x:default']) {
  if (capability.permissions.includes(forbiddenPermission)) {
    throw new Error(`Release capabilities contain broad or unused permission: ${forbiddenPermission}`);
  }
}

if (cargoText.includes('tauri-plugin-notification')) {
  throw new Error('The unused notification plugin must not return to the release dependency graph');
}

if (cargoText.includes('tauri-plugin-clipboard-x')) {
  throw new Error('Clipboard restore must remain in the Rust core without the broad Tauri clipboard plugin');
}

if (JSON.parse(packageText).dependencies?.['@tauri-apps/plugin-clipboard-manager']) {
  throw new Error('The unused JavaScript clipboard-manager plugin must not return');
}

for (const dependency of ['aes-gcm', 'hmac']) {
  if (!cargoText.includes(`${dependency} =`)) {
    throw new Error(`Encrypted storage requires the Rust ${dependency} dependency`);
  }
}

if (
  cargoText.includes('protocol-asset') ||
  clipCardSource.includes('convertFileSrc') ||
  tauriConfig.app?.security?.assetProtocol?.enable
) {
  throw new Error('Release builds must not expose stored image files through the WebView asset protocol');
}

for (const encryptedStorageGate of [
  'CryptProtectData',
  'Aes256Gcm',
  'keyed_hash',
  'storage_encryption_version',
  'migrate_encrypted_storage',
]) {
  const sources = `${cryptoSource}\n${databaseSource}\n${commandSource}\n${clipboardSource}`;
  if (!sources.includes(encryptedStorageGate)) {
    throw new Error(`Encrypted-storage release gate is missing: ${encryptedStorageGate}`);
  }
}

for (const clipboardFormatGate of [
  'clip_formats',
  'get_html()',
  'get_rich_text()',
  'get_files()',
  'ClipboardContent::Html',
  'ClipboardContent::Rtf',
  'ClipboardContent::Files',
]) {
  const sources = `${databaseSource}\n${commandSource}\n${clipboardSource}`;
  if (!sources.includes(clipboardFormatGate)) {
    throw new Error(`Multi-format clipboard release gate is missing: ${clipboardFormatGate}`);
  }
}

for (const sensitiveLogFragment of ['Detected self-paste for hash', 'full_path: {:?}', 'path match): {}']) {
  if (clipboardSource.includes(sensitiveLogFragment)) {
    throw new Error(`Clipboard source contains privacy-sensitive production logging: ${sensitiveLogFragment}`);
  }
}

for (const inheritedIdentity of ['PastePaw', 'XueshiQiao.PastePaw', 'XueshiQiao.github.io']) {
  if (releaseWorkflow.includes(inheritedIdentity)) {
    throw new Error(`Release workflow still contains inherited identity: ${inheritedIdentity}`);
  }
}

console.log(`Cubby Clipboard v${version} release metadata is consistent.`);
