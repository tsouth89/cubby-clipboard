/** Accessible-name i18n key for a flyout context menu.
 *
 *  Folder and History share `ContextMenu` with clip cards. Mapping by `kind`
 *  keeps those menus from being announced as clip actions (SBS-1013).
 */
export type ContextMenuKind = 'card' | 'folder' | 'history';

export function contextMenuLabelKey(
  kind: ContextMenuKind
): 'common.clipActions' | 'common.folderActions' | 'common.historyActions' {
  switch (kind) {
    case 'folder':
      return 'common.folderActions';
    case 'history':
      return 'common.historyActions';
    case 'card':
      return 'common.clipActions';
  }
}
