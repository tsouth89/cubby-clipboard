# Accessibility pass

A repeatable pass over the flyout and Settings window. Two halves:

- **Static audit** — semantics readable from the source. Anyone can redo it, no
  Windows session needed. Current results are recorded below.
- **Assistive-technology pass** — Narrator, scaling, contrast, and motion. These
  need a real Windows 11 desktop and cannot be inferred from the code.

Never put clipboard content or other sensitive data in test evidence. Copy
throwaway strings (`test-one`, `test-two`) and crop screenshots to the control
under test.

## Recording a run

Copy this block into the results section and fill it in:

```text
Date:            YYYY-MM-DD
Windows version: (winver → e.g. 11 24H2 26100.1234)
Cubby version:   (Settings → About)
Display:         (resolution, scaling %, text scaling %)
Theme:           (light | dark | high-contrast <name>)
Narrator:        (version / build if relevant)
Result:          pass | fail per checklist item
```

## Static audit checklist

Re-runnable with grep; no app session required.

1. Every interactive element has an accessible name — visible text, `aria-label`,
   or `aria-labelledby`. A `placeholder` is **not** an accessible name.
2. Selection state is exposed with the role that carries it (`option` +
   `aria-selected`, `tab` + `aria-selected`, `switch` + `aria-checked`).
   `aria-current` on a `listitem` does not announce selection.
3. Every `aria-modal="true"` container also moves focus into itself on open,
   traps Tab within itself, and restores focus to the invoking control on close.
   `aria-modal` without a trap is worse than no `aria-modal`, because it tells
   the screen reader the background is inert when it is not.
4. Composite widgets carry their full role set: `menu`/`menuitem`,
   `tablist`/`tab`/`tabpanel`, `listbox`/`option`.
5. Status changes that produce no focus change are announced through a live
   region.
6. `prefers-reduced-motion` and `forced-colors` blocks exist and cover the
   window chrome, list rows, and selection colors.

## Assistive-technology checklist

Needs a Windows 11 desktop. Start Narrator with `Ctrl+Win+Enter`.

### Narrator

- Opening the flyout announces the window and lands focus somewhere sensible.
- Arrowing through history announces each clip's position and selection state
  ("N of M", "selected"), not just its text.
- Pin, copy, and overflow buttons announce their name and pressed state.
- Copy, delete, and pin announce their outcome without moving focus.
- The context menu announces itself as a menu and its items as menu items.
- Settings tabs announce as tabs with the selected one marked.
- Dialogs announce their title on open, and Escape returns focus where it was.

### Keyboard only

Unplug or ignore the mouse for this section.

- The configured hotkey opens the flyout and focus is immediately usable.
- Tab order matches visual order, with no traps outside modal dialogs.
- Every control reachable by mouse is reachable by keyboard, including the
  context menu.
- Focus is always visible against the current theme.
- Escape closes menus and dialogs, one layer at a time.

### Scaling

Test at 100%, 150%, and 200% display scaling, and separately at 200% text
scaling (Settings → Accessibility → Text size).

- No clipped or overlapping text in the flyout header or Settings. (The
  `ControlBar` component is not rendered — see finding 8 — so there is no
  control bar to check until that is resolved.)
- The flyout still fits its monitor's work area and stays on screen.
- Scroll works when content exceeds the window.

### Contrast and motion

- Each Windows high-contrast theme: window chrome, list rows, the selected row,
  buttons, and inputs all remain distinguishable.
- Light and dark themes both meet contrast on body text and secondary text.
- With "Show animations in Windows" off, nothing animates.

## Results

### 2026-08-05 — static audit

Cubby 1.2.6. Static half only; the assistive-technology half has not been run
and is what issue #45 still needs.

Passing:

- `prefers-reduced-motion` and `forced-colors` blocks exist in
  `frontend/src/index.css` and cover the app window, clip cards, selected-row
  colors, and form controls.
- The show/hide paths in `lib.rs` do not animate despite their `animate_*`
  names, so there is no native motion outside CSS control.
- `ConfirmDialog` and `FolderModal` both carry `role="dialog"`,
  `aria-modal="true"`, and `aria-labelledby`, and both handle Escape.
- `FolderModal` moves focus to its input on open.
- Settings toggles use `role="switch"` with `aria-checked`.
- `ContextMenu` items are real `<button>` elements, so they are focusable and
  activatable, and it handles Escape.
- Icon-only ControlBar buttons have `aria-label` (added in #130).
- Status changes are announced. `sonner` renders its toast container with
  `aria-live="polite"`, and copy, delete, and pin all raise a toast.

Findings, worst first:

| # | Component | Finding | Checklist |
|---|---|---|---|
| 1 | `ClipList` / `ClipCard` | `role="list"` + `role="listitem"` with `aria-current` for the selected clip. The list is keyboard-navigated with a moving selection, so it needs `listbox` + `option` + `aria-selected`. As written, Narrator announces neither selection nor position. | 2 |
| ~~2~~ | `SearchBar` | **Withdrawn.** Recorded against `SearchBar.tsx`, which is not rendered anywhere. The live search input is in `FlyoutHeader`, and it already has `aria-label` on both the field and its clear button. | 1 |
| 3 | `WelcomeOverlay` | No `role`, no accessible name, and no Escape, on what is the first surface a new user sees. It does carry `autoFocus` on its dismiss button, so focus does land inside it. | 3 |
| 4 | `ConfirmDialog` | Declares `aria-modal="true"` but never moves focus into itself. None of the three dialogs trap Tab or restore focus to the invoking control on close. | 3 |
| 5 | `ContextMenu` | No `role="menu"` / `role="menuitem"`, no arrow-key navigation, and focus is not moved into the menu on open, so reaching it means tabbing through the page. | 4 |
| 6 | `SettingsPanel` | Tabs are plain buttons with no `role="tablist"` / `role="tab"` / `aria-selected`, so their tab nature and selected state are not conveyed. | 4 |
| ~~7~~ | App-wide | **Withdrawn.** Recorded as "no `aria-live` region anywhere", which was wrong: grepping the app source found none, but `sonner` renders one in its own container, and copy, delete, and pin all raise toasts through it. | 5 |

| 8 | `SearchBar`, `ControlBar`, `DragPreview` | None of these three components is imported anywhere. They are dead code, and auditing them produced two of the false positives above. `ControlBar` matters most: PR #130 added accessible names to its icon-only buttons, so that accessibility work never reached users. | — |

Fixed on branch `a11y/flyout-accessibility-pass`: 1, 3, 4, 5, 6. Findings 2 and 7
were withdrawn as false positives. Finding 8 is a live-code question rather than
an accessibility defect and wants its own decision.

Lesson worth keeping: grep the render tree before auditing a component. Two of
seven findings were against code that does not ship, and the audit only caught it
when wiring `aria-activedescendant` revealed `SearchBar` had no call site.

The assistive-technology half of this document is still unrun, and that is what
issue #45 needs next. The listbox change in finding 1 is the item most worth
confirming with Narrator, since `aria-activedescendant` is exactly the kind of
thing that reads correctly in the DOM and still fails in practice.
