import { FolderItem } from '../types';
import { ChevronDown, Filter, MoreHorizontal, Plus, Search, Settings2, X } from 'lucide-react';

export type ContentFilter = 'all' | 'text' | 'images' | 'files';

interface FlyoutHeaderProps {
  searchQuery: string;
  onSearchChange: (query: string) => void;
  contentFilter: ContentFilter;
  onContentFilterChange: (filter: ContentFilter) => void;
  folders: FolderItem[];
  selectedFolder: string | null;
  onSelectFolder: (folderId: string | null) => void;
  onAddFolder: () => void;
  onOpenHistoryMenu: (event: React.MouseEvent<HTMLButtonElement>) => void;
  onOpenSettings: () => void;
}

export function FlyoutHeader({
  searchQuery,
  onSearchChange,
  contentFilter,
  onContentFilterChange,
  folders,
  selectedFolder,
  onSelectFolder,
  onAddFolder,
  onOpenHistoryMenu,
  onOpenSettings,
}: FlyoutHeaderProps) {
  return (
    <header className="drag-area shrink-0 px-3 pb-2 pt-3">
      <div className="no-drag flex h-10 items-center gap-2 rounded-[10px] border border-white/[0.08] bg-white/[0.055] px-3 transition-colors focus-within:border-primary/45 focus-within:bg-white/[0.07]">
        <Search size={16} className="shrink-0 text-muted-foreground" />
        <input
          data-el="search-input"
          value={searchQuery}
          onChange={(event) => onSearchChange(event.target.value)}
          placeholder="Search clipboard history"
          className="min-w-0 flex-1 bg-transparent text-[13px] text-foreground outline-none placeholder:text-muted-foreground"
        />
        {searchQuery && (
          <button
            type="button"
            onClick={() => onSearchChange('')}
            className="rounded p-1 text-muted-foreground hover:bg-white/10 hover:text-foreground"
            aria-label="Clear search"
          >
            <X size={14} />
          </button>
        )}
      </div>

      <div className="no-drag mt-2 flex h-9 items-center gap-1">
        {(
          [
            ['all', 'All'],
            ['text', 'Text'],
            ['images', 'Images'],
            ['files', 'Files'],
          ] as const
        ).map(([id, label]) => (
          <button
            key={id}
            type="button"
            onClick={() => onContentFilterChange(id)}
            aria-pressed={contentFilter === id}
            className={`relative h-full px-2.5 text-xs font-medium transition-colors ${
              contentFilter === id
                ? 'text-foreground'
                : 'text-muted-foreground hover:text-foreground'
            }`}
          >
            {label}
            {contentFilter === id && (
              <span className="absolute inset-x-2 bottom-0 h-0.5 rounded-full bg-primary" />
            )}
          </button>
        ))}

        <div className="ml-auto flex items-center gap-0.5">
          <label className="relative flex items-center">
            <Filter
              size={14}
              className="pointer-events-none absolute left-2 text-muted-foreground"
            />
            <select
              value={selectedFolder ?? ''}
              onChange={(event) => onSelectFolder(event.target.value || null)}
              className="h-8 max-w-[132px] appearance-none rounded-md border border-transparent bg-transparent py-0 pl-7 pr-6 text-xs text-muted-foreground outline-none transition-colors hover:border-white/[0.08] hover:bg-white/[0.05] hover:text-foreground"
              aria-label="Filter by folder"
            >
              <option value="">All folders</option>
              {folders.map((folder) => (
                <option key={folder.id} value={folder.id}>
                  {folder.name}
                </option>
              ))}
            </select>
            <ChevronDown
              size={12}
              className="pointer-events-none absolute right-2 text-muted-foreground"
            />
          </label>
          <button
            type="button"
            onClick={onAddFolder}
            className="rounded-md p-2 text-muted-foreground transition-colors hover:bg-white/[0.07] hover:text-foreground"
            title="New folder"
          >
            <Plus size={15} />
          </button>
          <button
            type="button"
            onClick={onOpenHistoryMenu}
            className="rounded-md p-2 text-muted-foreground transition-colors hover:bg-white/[0.07] hover:text-foreground"
            title="Clipboard history actions"
            aria-label="Clipboard history actions"
          >
            <MoreHorizontal size={15} />
          </button>
          <button
            type="button"
            onClick={onOpenSettings}
            className="rounded-md p-2 text-muted-foreground transition-colors hover:bg-white/[0.07] hover:text-foreground"
            title="Settings"
          >
            <Settings2 size={15} />
          </button>
        </div>
      </div>
    </header>
  );
}
