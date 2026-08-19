import { describe, expect, it } from 'vitest';
import { isImeKey, shortcutsSuspended, shouldCaptureTypeToSearch } from './flyoutSearch';

function key(
  partial: Partial<{
    isComposing: boolean;
    key: string;
    keyCode: number;
    ctrlKey: boolean;
    altKey: boolean;
    metaKey: boolean;
  }>
) {
  return {
    isComposing: false,
    key: 'a',
    ctrlKey: false,
    altKey: false,
    metaKey: false,
    ...partial,
  };
}

describe('isImeKey', () => {
  it('treats composing, Process, and keyCode 229 as IME', () => {
    expect(isImeKey(key({ isComposing: true }))).toBe(true);
    expect(isImeKey(key({ key: 'Process' }))).toBe(true);
    expect(isImeKey(key({ keyCode: 229 }))).toBe(true);
    expect(isImeKey(key({ key: 'a' }))).toBe(false);
  });
});

describe('shouldCaptureTypeToSearch', () => {
  it('captures a bare printable character', () => {
    expect(shouldCaptureTypeToSearch(key({ key: 'a' }))).toBe(true);
    expect(shouldCaptureTypeToSearch(key({ key: 'あ' }))).toBe(true);
  });

  it('ignores IME so composition is not preventDefaulted into Latin', () => {
    expect(shouldCaptureTypeToSearch(key({ isComposing: true, key: 'a' }))).toBe(false);
    expect(shouldCaptureTypeToSearch(key({ key: 'Process' }))).toBe(false);
    expect(shouldCaptureTypeToSearch(key({ key: 'a', keyCode: 229 }))).toBe(false);
  });

  it('ignores modifiers and non-character keys', () => {
    expect(shouldCaptureTypeToSearch(key({ ctrlKey: true }))).toBe(false);
    expect(shouldCaptureTypeToSearch(key({ altKey: true }))).toBe(false);
    expect(shouldCaptureTypeToSearch(key({ metaKey: true }))).toBe(false);
    expect(shouldCaptureTypeToSearch(key({ key: 'Enter' }))).toBe(false);
    expect(shouldCaptureTypeToSearch(key({ key: 'Escape' }))).toBe(false);
  });
});

describe('shortcutsSuspended', () => {
  it('stands down while a modal confirm owns the keyboard', () => {
    // ConfirmDialog listens on window, which runs after document, so a
    // History shortcut that keeps acting answers the confirm for the user:
    // Delete hard-deletes the previewed clip mid-question (SBS-1007).
    expect(shortcutsSuspended(key({ key: 'Delete' }), true)).toBe(true);
    expect(shortcutsSuspended(key({ key: 'Escape' }), true)).toBe(true);
    expect(shortcutsSuspended(key({ key: 'Enter' }), true)).toBe(true);
  });

  it('lets shortcuts through with no modal open', () => {
    expect(shortcutsSuspended(key({ key: 'Delete' }), false)).toBe(false);
    expect(shortcutsSuspended(key({ key: 'Escape' }), false)).toBe(false);
  });

  it('still stands down for IME composition with no modal open', () => {
    expect(shortcutsSuspended(key({ isComposing: true, key: 'Enter' }), false)).toBe(true);
    expect(shortcutsSuspended(key({ keyCode: 229, key: 'Process' }), false)).toBe(true);
  });
});
