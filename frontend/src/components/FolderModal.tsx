import { useEffect, useId, useRef, useState } from 'react';
import { useModalDialog } from '../hooks/useModalDialog';

interface FolderModalProps {
  isOpen: boolean;
  mode: 'create' | 'rename';
  initialName: string;
  onClose: () => void;
  onSubmit: (name: string) => void;
}

export function FolderModal({ isOpen, mode, initialName, onClose, onSubmit }: FolderModalProps) {
  const inputRef = useRef<HTMLInputElement>(null);
  const [isSubmitting, setIsSubmitting] = useState(false);
  const titleId = useId();
  const dialogRef = useModalDialog<HTMLDivElement>(onClose, isOpen);

  useEffect(() => {
    if (isOpen) {
      setIsSubmitting(false); // Reset on open
      if (inputRef.current) {
        // slight delay to ensure render
        setTimeout(() => inputRef.current?.focus(), 50);
        // If rename, select all text
        if (mode === 'rename') {
          setTimeout(() => inputRef.current?.select(), 50);
        }
      }
    }
  }, [isOpen, mode]);

  if (!isOpen) return null;

  const handleSubmit = async () => {
    if (isSubmitting) return;
    const val = inputRef.current?.value.trim();
    if (val) {
      setIsSubmitting(true);
      await onSubmit(val);
      setIsSubmitting(false);
    }
  };

  return (
    <div className="absolute inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm">
      <div
        ref={dialogRef}
        tabIndex={-1}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        className="w-80 rounded-2xl border border-border bg-card p-6 shadow-2xl"
      >
        <h3 id={titleId} className="mb-4 text-lg font-semibold text-foreground">
          {mode === 'create' ? 'Create Folder' : 'Rename Folder'}
        </h3>
        <input
          ref={inputRef}
          type="text"
          placeholder={'Folder name'}
          aria-label={'Folder name'}
          defaultValue={initialName}
          className="mb-4 w-full rounded-md border border-input bg-input px-3 py-2 text-sm text-foreground focus:border-primary focus:outline-none focus:ring-1 focus:ring-primary"
          onKeyDown={(e) => {
            // Escape is handled dialog-wide by useModalDialog, so it works from
            // the buttons too, not only while the input has focus.
            if (e.key === 'Enter' && !e.nativeEvent.isComposing) {
              handleSubmit();
            }
          }}
        />
        <div className="flex justify-end gap-2">
          <button
            onClick={onClose}
            disabled={isSubmitting}
            className="rounded-md px-3 py-1.5 text-sm font-medium text-muted-foreground hover:bg-secondary hover:text-foreground disabled:opacity-50"
          >
            {'Cancel'}
          </button>
          <button
            onClick={handleSubmit}
            disabled={isSubmitting}
            className="rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
          >
            {isSubmitting ? 'Loading...' : mode === 'create' ? 'Create' : 'Save'}
          </button>
        </div>
      </div>
    </div>
  );
}
