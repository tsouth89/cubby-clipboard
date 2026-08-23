/** Accessible name for a flyout context menu. */
export type ContextMenuKind = 'card' | 'folder' | 'history';

export function contextMenuLabel(kind: ContextMenuKind): string {
  switch (kind) {
    case 'folder':
      return 'Folder actions';
    case 'history':
      return 'History actions';
    case 'card':
      return 'Clip actions';
  }
}
