import { describe, expect, it } from 'vitest';
import { classifyKeyboardTarget } from './keyboardTarget';

describe('classifyKeyboardTarget', () => {
  it('recognizes native controls and descendants of buttons and links', () => {
    const host = document.createElement('div');
    host.innerHTML = '<button><span>Save</span></button><select></select><a href="#">Help</a>';

    for (const target of host.querySelectorAll('span, select, a')) {
      expect(classifyKeyboardTarget(target, '[data-el="search-input"]').isInteractive).toBe(true);
    }
  });

  it('recognizes applicable ARIA widgets', () => {
    for (const role of ['button', 'combobox', 'listbox', 'menuitem', 'option', 'switch', 'tab']) {
      const target = document.createElement('div');
      target.setAttribute('role', role);
      expect(classifyKeyboardTarget(target, '[data-el="search-input"]').isInteractive).toBe(true);
    }
  });

  it('keeps search inputs identifiable while treating them as editing controls', () => {
    const search = document.createElement('input');
    search.dataset.el = 'search-input';
    expect(classifyKeyboardTarget(search, '[data-el="search-input"]')).toEqual({
      isEditing: true,
      isInteractive: true,
      isSearch: true,
    });
  });
});
