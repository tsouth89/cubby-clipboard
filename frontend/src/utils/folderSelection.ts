/** Keep the current folder if it still exists; otherwise fall back to All. */
export function folderSelectionAfterReload(
  selectedFolder: string | null,
  folders: { id: string }[]
): string | null {
  if (selectedFolder && !folders.some((folder) => folder.id === selectedFolder)) {
    return null;
  }
  return selectedFolder;
}
