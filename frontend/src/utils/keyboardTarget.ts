const INTERACTIVE_SELECTOR = [
  'button',
  'select',
  'a[href]',
  '[contenteditable="true"]',
  '[role="button"]',
  '[role="checkbox"]',
  '[role="combobox"]',
  '[role="listbox"]',
  '[role="menuitem"]',
  '[role="option"]',
  '[role="radio"]',
  '[role="slider"]',
  '[role="switch"]',
  '[role="tab"]',
  '[role="textbox"]',
].join(',');

export interface KeyboardTargetState {
  isEditing: boolean;
  isInteractive: boolean;
  isSearch: boolean;
}

export function classifyKeyboardTarget(
  target: EventTarget | null,
  searchSelector: string
): KeyboardTargetState {
  if (!(target instanceof Element)) {
    return { isEditing: false, isInteractive: false, isSearch: false };
  }

  const isEditing =
    target.closest('input, textarea, [contenteditable="true"], [role="textbox"]') !== null;
  return {
    isEditing,
    isInteractive: isEditing || target.closest(INTERACTIVE_SELECTOR) !== null,
    isSearch: target.matches(searchSelector),
  };
}
