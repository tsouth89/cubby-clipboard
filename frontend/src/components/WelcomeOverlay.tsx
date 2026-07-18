import { Clipboard, Keyboard, Lock, Pin } from 'lucide-react';
import { useTranslation } from 'react-i18next';

interface WelcomeOverlayProps {
  onDismiss: () => void;
}

/**
 * First-run welcome shown over the flyout until the user dismisses it. Tells the
 * user Cubby is running, how to open it, and what it does. References Win+V,
 * which replaces the native flyout by default.
 */
export function WelcomeOverlay({ onDismiss }: WelcomeOverlayProps) {
  const { t } = useTranslation();

  const points = [
    { icon: Keyboard, text: t('onboarding.pointOpen') },
    { icon: Clipboard, text: t('onboarding.pointAuto') },
    { icon: Pin, text: t('onboarding.pointUse') },
    { icon: Lock, text: t('onboarding.pointPrivate') },
  ];

  return (
    <div className="animate-in fade-in absolute inset-0 z-50 flex items-center justify-center bg-black/60 p-3 backdrop-blur-sm duration-200">
      <div className="animate-in zoom-in-95 flex max-h-full w-full max-w-sm flex-col overflow-hidden rounded-lg border border-border bg-background shadow-lg duration-200">
        <div className="flex flex-col gap-1 px-5 pt-5">
          <h2 className="text-base font-semibold text-foreground">{t('onboarding.title')}</h2>
          <p className="text-xs text-muted-foreground">{t('onboarding.intro')}</p>
        </div>
        <ul className="flex flex-col gap-3 overflow-y-auto px-5 py-4">
          {points.map(({ icon: Icon, text }, index) => (
            <li key={index} className="flex items-start gap-3">
              <div className="mt-0.5 flex h-6 w-6 shrink-0 items-center justify-center rounded-md bg-primary/10 text-primary">
                <Icon size={14} />
              </div>
              <span className="text-xs leading-relaxed text-foreground/90">{text}</span>
            </li>
          ))}
        </ul>
        <div className="px-5 pb-5">
          <button
            onClick={onDismiss}
            autoFocus
            className="w-full rounded-md bg-primary px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-primary/90"
          >
            {t('onboarding.dismiss')}
          </button>
        </div>
      </div>
    </div>
  );
}
