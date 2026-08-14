import { describe, expect, it } from 'vitest';
import { isImeKey, shouldCaptureTypeToSearch } from './flyoutSearch';

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
