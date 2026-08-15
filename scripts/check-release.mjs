import { readdir, readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { checkPrivilegedActionPins } from '../.github/scripts/check-privileged-action-pins.mjs';
import { findFileListHistoryClaims } from './product-page-claims.mjs';
import { extractDefaultSkipLikelySecrets, evaluateSecretHeuristicsDoc, saysDefaultOff } from './release-check-helpers.mjs';

const root = new URL('../', import.meta.url);
const rootDir = fileURLToPath(root);
const read = (relativePath) => readFile(new URL(relativePath, root), 'utf8');

const [
  packageText,
  tauriText,
  storeTauriText,
  cargoText,
  changelog,
  releaseWorkflow,
  capabilityText,
  clipboardSource,
  cryptoSource,
  databaseSource,
  commandSource,
  clipCardSource,
  secretsSource,
  modelsSource,
  securityDoc,
  readmeDoc,
  privacyPageDoc,
  supportPageDoc,
  settingsPanelSource,
  settingsLocaleText,
  publishStoreWorkflow,
  validateStoreWorkflow,
  verifyInstallerSignature,
  libSource,
  logTargetsSource,
] = await Promise.all([
  read('package.json'),
  read('src-tauri/tauri.conf.json'),
  read('src-tauri/tauri.store.conf.json'),
  read('src-tauri/Cargo.toml'),
  read('CHANGELOG.md'),
  read('.github/workflows/release.yml'),
  read('src-tauri/capabilities/default.json'),
  read('src-tauri/src/clipboard.rs'),
  read('src-tauri/src/crypto.rs'),
  read('src-tauri/src/database.rs'),
  read('src-tauri/src/commands.rs'),
  read('frontend/src/components/ClipCard.tsx'),
  read('src-tauri/src/secrets.rs'),
  read('src-tauri/src/models.rs'),
  read('SECURITY.md'),
  read('README.md'),
  read('product_pages/privacy.html'),
  read('product_pages/support.html'),
  read('frontend/src/components/SettingsPanel.tsx'),
  read('frontend/src/i18n/locales/en.json'),
  read('.github/workflows/publish-store-packages.yml'),
  read('.github/workflows/validate-store-submission.yml'),
  read('scripts/verify-installer-signature.ps1'),
  read('src-tauri/src/lib.rs'),
  read('src-tauri/src/log_targets.rs'),
]);

const packageVersion = JSON.parse(packageText).version;
const tauriConfig = JSON.parse(tauriText);
const storeTauriConfig = JSON.parse(storeTauriText);
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

if (
  storeTauriConfig.bundle?.windows?.webviewInstallMode?.type !== 'offlineInstaller'
) {
  throw new Error('Microsoft Store installers must embed the offline WebView2 installer');
}

if (storeTauriConfig.bundle?.createUpdaterArtifacts !== false) {
  throw new Error('Microsoft Store builds must not generate updater artifacts');
}

if (!releaseWorkflow.includes('--config src-tauri/tauri.store.conf.json')) {
  throw new Error('Release workflow must build the Microsoft Store installer with its offline configuration');
}

if (!releaseWorkflow.includes('--features app-store')) {
  throw new Error('Microsoft Store builds must disable Cubby self-update and autostart integration');
}

// SBS-777: a signed outer Store installer is not enough. Publication and
// validation must extract and check the packed cubby.exe and uninstall.exe.
for (const [name, source] of [
  ['publish-store-packages.yml', publishStoreWorkflow],
  ['validate-store-submission.yml', validateStoreWorkflow],
]) {
  if (!source.includes('verify-installer-signature.ps1')) {
    throw new Error(`${name} must verify embedded cubby and uninstaller signatures`);
  }
}
if (!verifyInstallerSignature.includes('uninstall.exe')) {
  throw new Error('verify-installer-signature.ps1 must still inspect uninstall.exe when 7-Zip extracts it');
}

const csp = tauriConfig.app?.security?.csp;
if (typeof csp !== 'string' || !csp.includes("default-src 'self'") || !csp.includes("object-src 'none'")) {
  throw new Error('Release builds must use the restrictive Cubby content-security policy');
}

// Every window Cubby opens, listed explicitly. Kept an exact match rather than
// a subset check so a new window has to be added here deliberately instead of
// inheriting the app's capabilities by accident.
const allowedCapabilityWindows = ['main', 'settings', 'history', 'image'];
if (JSON.stringify(capability.windows) !== JSON.stringify(allowedCapabilityWindows)) {
  throw new Error(
    `Tauri capabilities must be scoped to exactly: ${allowedCapabilityWindows.join(', ')}`
  );
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
  'ClipboardContent::Html',
  'ClipboardContent::Rtf',
]) {
  const sources = `${databaseSource}\n${commandSource}\n${clipboardSource}`;
  if (!sources.includes(clipboardFormatGate)) {
    throw new Error(`Multi-format clipboard release gate is missing: ${clipboardFormatGate}`);
  }
}

for (const [source, fileHistoryGate] of [
  [clipboardSource, 'clipboard_has_file_payload_format()'],
  [databaseSource, "DELETE FROM clips WHERE clip_type IN ('file', 'files')"],
]) {
  if (!source.includes(fileHistoryGate)) {
    throw new Error(`File-history removal gate is missing: ${fileHistoryGate}`);
  }
}

if (`${clipboardSource}\n${commandSource}`.includes('ClipboardContent::Files')) {
  throw new Error(
    'Release product code must not restore external file references as durable history'
  );
}

// The gates above encode that copied files are never stored. Public docs claimed
// the opposite for months because nothing tied the two together. If file capture
// is ever restored, delete these lines deliberately along with the design work.
//
// A sentence only counts as a claim if it says files are retained or
// supported (retained|stores|supports|includes|records) without also
// negating that (not|never|ignore). This lets docs correctly say "Cubby does
// not store file lists" while still catching restated lies like "Cubby
// stores copied files" that don't use the literal phrase "file lists".
// Checked per sentence rather than per line: a paragraph line can carry both
// an accurate negated claim and a false unnegated one, and a negation later
// in the same line must not paper over a false claim earlier in it.
const fileMentionPattern = /\bfiles?\b/i;
const retentionClaimPattern = /\b(?:retained|stores?|supports?|includes?|records?(?:ed)?)\b/i;
const negationPattern = /\b(?:not|never|ignor\w*)\b/i;
for (const [docName, doc] of [
  ['README.md', readmeDoc],
  ['SECURITY.md', securityDoc],
  ['product_pages/privacy.html', privacyPageDoc],
  ['product_pages/support.html', supportPageDoc],
]) {
  const fileClaim = doc
    .replace(/\r?\n/g, ' ')
    .split(/(?<=[.!?])\s+/)
    .find(
      (sentence) =>
        fileMentionPattern.test(sentence) &&
        retentionClaimPattern.test(sentence) &&
        !negationPattern.test(sentence)
    );
  if (fileClaim) {
    throw new Error(
      `${docName} claims file clipboard history is retained or supported, which capture policy deliberately ignores: ${fileClaim.trim()}`
    );
  }
}

// A URL the frontend opens but the capability does not allow is rejected at the
// Tauri boundary, which reads to the user as a button that does nothing.
const openerScope = capability.permissions.find(
  (permission) => permission?.identifier === 'opener:allow-open-url'
);
// No `$` anchor: these sources are CRLF, so a trailing `\r` would leave the
// pattern matching nothing and the gate passing on an empty set.
const allowedUrls = new Set((openerScope?.allow ?? []).map((entry) => entry.url));
const openedUrls = [...settingsPanelSource.matchAll(/^const \w*URL = '([^']+)';/gm)].map(
  ([, url]) => url
);
if (openedUrls.length === 0) {
  throw new Error('Could not find the Settings link URL constants to check against the allowlist');
}
for (const openedUrl of openedUrls) {
  if (!allowedUrls.has(openedUrl)) {
    throw new Error(
      `Settings opens ${openedUrl}, but src-tauri/capabilities/default.json does not allow it`
    );
  }
}

for (const sensitiveLogFragment of ['Detected self-paste for hash', 'full_path: {:?}', 'path match): {}']) {
  if (clipboardSource.includes(sensitiveLogFragment)) {
    throw new Error(`Clipboard source contains privacy-sensitive production logging: ${sensitiveLogFragment}`);
  }
}

// SBS-837: release builds must not stream Rust logs into the WebView.
// Prefer an inline `#[cfg(not(debug_assertions))]` targets list when present;
// otherwise the production arm of `log_targets()` is the source of truth.
const inlineReleaseTargets = libSource.match(
  /#\[cfg\(not\(debug_assertions\)\)\][\s\S]*?\.targets\(\[([\s\S]*?)\]\)/,
)?.[1];
if (inlineReleaseTargets?.includes('Webview')) {
  throw new Error('Release log_builder.targets must not include Webview');
}

if (!libSource.includes('log_targets(cfg!(debug_assertions))')) {
  throw new Error(
    'lib.rs must install logger targets from log_targets(cfg!(debug_assertions))',
  );
}

const productionLogTargets = logTargetsSource.match(
  /if debug_assertions \{[\s\S]*?\} else \{([\s\S]*?)\}/,
)?.[1];
if (!productionLogTargets) {
  throw new Error('Could not find the production arm of log_targets()');
}
if (productionLogTargets.includes('Webview')) {
  throw new Error('Release log_builder.targets must not include Webview');
}
if (!productionLogTargets.includes('LogDir')) {
  throw new Error('Release log_builder.targets must still include LogDir');
}

const secretGates = [
  [secretsSource, 'classify_secret'],
  [secretsSource, 'DEFAULT_SENSITIVE_APP_EXES'],
  [modelsSource, 'skip_likely_secrets'],
  [modelsSource, 'default_sensitive_apps_seeded'],
  [clipboardSource, 'settings.skip_likely_secrets'],
  [clipboardSource, 'crate::secrets::classify_secret'],
];
for (const [source, gate] of secretGates) {
  if (!source.includes(gate)) {
    throw new Error(`Secret-aware privacy release gate is missing: ${gate}`);
  }
}

// SBS-811: SECURITY.md once said secret heuristics were "default on" while
// AppSettings::default set skip_likely_secrets: false. Pin the shipped default
// to the security page and Settings copy so they cannot drift again.
const defaultSkipLikelySecrets = extractDefaultSkipLikelySecrets(modelsSource);
if (defaultSkipLikelySecrets !== 'true' && defaultSkipLikelySecrets !== 'false') {
  throw new Error('Could not read skip_likely_secrets from AppSettings::default');
}

const {
  bullets: secretHeuristicBullets,
  sayOn: securitySaysOn,
  sayOff: securitySaysOff,
} = evaluateSecretHeuristicsDoc(securityDoc);
if (secretHeuristicBullets.length === 0) {
  throw new Error('SECURITY.md must document high-confidence secret heuristics');
}

if (defaultSkipLikelySecrets === 'false' && (securitySaysOn || !securitySaysOff)) {
  throw new Error(
    `Shipped skip_likely_secrets default is off; SECURITY.md must say off by default / opt-in, not default on: ${secretHeuristicBullets.join(' | ')}`
  );
}
if (defaultSkipLikelySecrets === 'true' && (!securitySaysOn || securitySaysOff)) {
  throw new Error(
    `Shipped skip_likely_secrets default is on; SECURITY.md must say default on: ${secretHeuristicBullets.join(' | ')}`
  );
}

const skipLikelySecretsDesc = JSON.parse(settingsLocaleText).settings?.skipLikelySecretsDesc;
if (typeof skipLikelySecretsDesc !== 'string') {
  throw new Error('Settings locale must describe skipLikelySecrets');
}
if (defaultSkipLikelySecrets === 'false' && !saysDefaultOff(skipLikelySecretsDesc)) {
  throw new Error('Settings copy must say secret heuristics are off by default');
}
if (defaultSkipLikelySecrets === 'true' && !/on by default/i.test(skipLikelySecretsDesc)) {
  throw new Error('Settings copy must say secret heuristics are on by default');
}

if (!securityDoc.includes('RUSTSEC-2023-0071')) {
  throw new Error('SECURITY.md must document the reviewed RSA advisory waiver');
}

const productPageSources = [
  ['product_pages/privacy.html', await read('product_pages/privacy.html')],
  ['product_pages/support.html', await read('product_pages/support.html')],
];
for (const [file, source] of productPageSources) {
  const [claim] = findFileListHistoryClaims(source);
  if (claim) {
    throw new Error(`${file} still claims file-list clipboard history: ${claim}`);
  }
}

const reviewed = securityDoc.match(/^- Reviewed:\s*(\d{4}-\d{2}-\d{2})\s*$/m)?.[1];
const nextReview = securityDoc.match(/^- Next review:\s*(\d{4}-\d{2}-\d{2})\b/m)?.[1];
const today = new Date().toISOString().slice(0, 10);

if (!reviewed || !nextReview || nextReview < today) {
  throw new Error(
    'SECURITY.md must contain current reviewed and next-review dates for the RSA waiver',
  );
}

async function collectFrontendSources(dir) {
  const entries = await readdir(dir, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const fullPath = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await collectFrontendSources(fullPath)));
      continue;
    }
    if (entry.name.endsWith('.ts') || entry.name.endsWith('.tsx')) {
      files.push(fullPath);
    }
  }
  return files;
}

const frontendFiles = await collectFrontendSources(path.join(rootDir, 'frontend', 'src'));
for (const filePath of frontendFiles) {
  const source = await readFile(filePath, 'utf8');
  if (source.includes('dangerouslySetInnerHTML')) {
    throw new Error(
      `Frontend must not use dangerouslySetInnerHTML (${path.relative(rootDir, filePath)})`
    );
  }
}

for (const inheritedIdentity of ['PastePaw', 'XueshiQiao.PastePaw', 'XueshiQiao.github.io']) {
  if (releaseWorkflow.includes(inheritedIdentity)) {
    throw new Error(`Release workflow still contains inherited identity: ${inheritedIdentity}`);
  }
}

const pinCheck = await checkPrivilegedActionPins(rootDir);
if (pinCheck.violations.length > 0) {
  const details = pinCheck.violations
    .map((item) => `${item.file}:${item.line}: ${item.reason} (${item.spec})`)
    .join('; ');
  throw new Error(`Privileged workflows have mutable third-party actions: ${details}`);
}

console.log(`Cubby Clipboard v${version} release metadata is consistent.`);
