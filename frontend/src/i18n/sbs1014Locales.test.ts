import { describe, expect, it } from 'vitest';

import de from './locales/de.json';
import en from './locales/en.json';
import fr from './locales/fr.json';
import ja from './locales/ja.json';
import zh from './locales/zh.json';

/**
 * SBS-1014: these keys shipped English-only to de/fr/ja/zh. Two distinct
 * failure modes: English copied verbatim, or the key omitted so
 * fallbackLng: 'en' shows English in an otherwise localized UI.
 */
const SBS_1014_KEYS = [
  'clipList.loadMoreFailed',
  'clipList.refreshFailed',
  'settings.linkFailed',
  'settings.portableUpdateNote',
  'common.clipActions',
] as const;

type Dict = Record<string, unknown>;

function lookup(tree: Dict, dotted: string): unknown {
  return dotted.split('.').reduce<unknown>((node, part) => {
    if (node && typeof node === 'object' && part in (node as Dict)) {
      return (node as Dict)[part];
    }
    return undefined;
  }, tree);
}

const locales = { de, fr, ja, zh } as const;

describe('SBS-1014 locale strings', () => {
  it('translates the five English-only keys instead of copying or omitting them', () => {
    // A German, French, Japanese, or Chinese user who hits a stale-list
    // refresh failure, a blocked Settings link, or the portable-update note
    // used to see English. Missing keys and byte-identical English both
    // produce that.
    for (const [name, catalog] of Object.entries(locales)) {
      for (const key of SBS_1014_KEYS) {
        const translated = lookup(catalog as Dict, key);
        const english = lookup(en as Dict, key);
        expect(translated, `${name} ${key} must exist`).toEqual(expect.any(String));
        expect(translated, `${name} ${key} must not be English`).not.toBe(english);
        expect((translated as string).trim().length, `${name} ${key} empty`).toBeGreaterThan(0);
      }
    }
  });

  it('keeps the {{url}} interpolation hole in settings.linkFailed', () => {
    // SettingsPanel calls t('settings.linkFailed', { url }). Translating the
    // hole away would drop the URL the user tried to open.
    for (const [name, catalog] of Object.entries(locales)) {
      const value = lookup(catalog as Dict, 'settings.linkFailed');
      expect(value, name).toEqual(expect.stringContaining('{{url}}'));
    }
  });

  it('keeps the portable filenames a user must not rename', () => {
    // The note tells the user to replace Cubby Clipboard.exe and keep the
    // data folder plus portable.txt. Those names are on disk.
    for (const [name, catalog] of Object.entries(locales)) {
      const value = lookup(catalog as Dict, 'settings.portableUpdateNote');
      expect(value, `${name} exe`).toEqual(expect.stringContaining('Cubby Clipboard.exe'));
      expect(value, `${name} portable.txt`).toEqual(expect.stringContaining('portable.txt'));
      // \bdata\b, not /data/: the latter is satisfied by "database" or
      // "metadata", so a translation that dropped the folder name would still
      // pass while telling the user to keep the wrong path. A word boundary
      // also survives the different quoting each locale uses around it
      // ('data', 「data」, “data”), which stringContaining("'data'") would not.
      expect(value, `${name} data folder`).toEqual(expect.stringMatching(/\bdata\b/));
    }
  });

  it('rejects a portable note that mentions data only inside another word', () => {
    // Guards the assertion above: it must fail for a note that names the exe
    // and portable.txt but never the 'data' folder itself.
    const impostor =
      'Ersetzen Sie Cubby Clipboard.exe. Die database und portable.txt bleiben erhalten.';
    expect(impostor).toEqual(expect.stringContaining('Cubby Clipboard.exe'));
    expect(impostor).toEqual(expect.stringContaining('portable.txt'));
    expect(impostor).toEqual(expect.stringMatching(/data/));
    expect(impostor).not.toEqual(expect.stringMatching(/\bdata\b/));
  });
});
