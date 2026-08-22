import { describe, expect, it } from 'vitest';
import de from '../i18n/locales/de.json';
import en from '../i18n/locales/en.json';
import fr from '../i18n/locales/fr.json';
import ja from '../i18n/locales/ja.json';
import zh from '../i18n/locales/zh.json';
import { contextMenuLabelKey } from './contextMenuLabel';

const locales = { de, en, fr, ja, zh } as const;

describe('contextMenuLabelKey', () => {
  it('does not announce folder or history menus as clip actions (SBS-1013)', () => {
    expect(contextMenuLabelKey('folder')).toBe('common.folderActions');
    expect(contextMenuLabelKey('history')).toBe('common.historyActions');
    expect(contextMenuLabelKey('card')).toBe('common.clipActions');
  });

  it('gives folder and history a distinct translated name in every locale', () => {
    for (const [name, locale] of Object.entries(locales)) {
      const { clipActions, folderActions, historyActions } = locale.common;
      expect(clipActions.length, name).toBeGreaterThan(0);
      expect(folderActions.length, name).toBeGreaterThan(0);
      expect(historyActions.length, name).toBeGreaterThan(0);
      expect(folderActions, name).not.toBe(clipActions);
      expect(historyActions, name).not.toBe(clipActions);
    }
  });
});
