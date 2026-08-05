import { X, AlertTriangle } from 'lucide-react';
import { useEffect, useId } from 'react';
import { useTranslation } from 'react-i18next';
import { useModalDialog } from '../hooks/useModalDialog';

interface ConfirmDialogProps {
  isOpen: boolean;
  title: string;
  message: string;
  confirmText?: string;
  cancelText?: string;
  onConfirm: () => void;
  onCancel: () => void;
  variant?: 'danger' | 'warning' | 'info';
  isBusy?: boolean;
}

export function ConfirmDialog({
  isOpen,
  title,
  message,
  confirmText,
  cancelText,
  onConfirm,
  onCancel,
  variant = 'danger',
  isBusy = false,
}: ConfirmDialogProps) {
  const { t } = useTranslation();
  const titleId = useId();
  const descriptionId = useId();
  // Escape is handled below (it has to respect isBusy), so the hook only
  // supplies focus entry, the Tab trap, and focus restore.
  const dialogRef = useModalDialog<HTMLDivElement>(undefined, isOpen);

  useEffect(() => {
    const handleEscape = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && !isBusy) onCancel();
    };
    if (isOpen) {
      window.addEventListener('keydown', handleEscape);
    }
    return () => window.removeEventListener('keydown', handleEscape);
  }, [isOpen, isBusy, onCancel]);

  if (!isOpen) return null;

  return (
    <div className="animate-in fade-in fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm duration-200">
      <div
        ref={dialogRef}
        tabIndex={-1}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={descriptionId}
        className="animate-in zoom-in-95 w-full max-w-md scale-100 rounded-lg border border-border bg-background p-6 shadow-lg duration-200"
      >
        <div className="mb-4 flex items-center justify-between">
          <div className="flex items-center gap-2">
            <div
              className={`flex h-8 w-8 items-center justify-center rounded-full ${
                variant === 'danger'
                  ? 'bg-destructive/10 text-destructive'
                  : 'bg-yellow-500/10 text-yellow-500'
              }`}
            >
              <AlertTriangle size={18} />
            </div>
            <h3 id={titleId} className="text-lg font-semibold">
              {title}
            </h3>
          </div>
          <button
            onClick={onCancel}
            disabled={isBusy}
            aria-label={t('common.cancel')}
            className="text-muted-foreground hover:text-foreground disabled:opacity-40"
          >
            <X size={18} />
          </button>
        </div>

        <p id={descriptionId} className="mb-6 text-sm text-muted-foreground">
          {message}
        </p>

        <div className="flex justify-end gap-3">
          <button
            onClick={onCancel}
            disabled={isBusy}
            autoFocus
            className="rounded-md border border-input bg-transparent px-4 py-2 text-sm font-medium hover:bg-accent hover:text-accent-foreground disabled:opacity-40"
          >
            {cancelText || t('common.cancel')}
          </button>
          <button
            onClick={onConfirm}
            disabled={isBusy}
            className={`rounded-md px-4 py-2 text-sm font-medium text-white transition-colors disabled:opacity-60 ${
              variant === 'danger'
                ? 'bg-destructive hover:bg-destructive/90'
                : 'bg-primary hover:bg-primary/90'
            }`}
          >
            {confirmText || t('common.confirm')}
          </button>
        </div>
      </div>
    </div>
  );
}
