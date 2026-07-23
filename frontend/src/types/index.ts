export interface ClipboardItem {
  id: string;
  clip_type: string;
  content: string;
  preview: string;
  folder_id: string | null;
  is_pinned: boolean;
  created_at: string;
  source_app: string | null;
  source_icon: string | null;
  metadata: string | null;
  has_ocr_text?: boolean;
  ocr_match: OcrMatch | null;
  // Retention dropped this image's full-resolution blob but kept its thumbnail
  // and OCR text (SOU-244). It stays searchable; the full image can't be pasted.
  image_expired?: boolean;
}

export interface OcrMatch {
  before: string;
  matched: string;
  after: string;
}

export interface FolderItem {
  id: string;
  name: string;
  icon: string | null;
  color: string | null;
  is_system: boolean;
  item_count: number;
}

export interface Settings {
  max_items: number;
  auto_delete_days: number;
  startup_with_windows: boolean;
  is_portable?: boolean;
  show_in_taskbar: boolean;
  hotkey: string;
  replace_win_v: boolean;
  theme: string;
  language?: string;
  mica_effect?: string;
  round_corners?: boolean;
  float_above_taskbar?: boolean;
  density?: 'compact' | 'comfortable';
  remote_paste_mode: 'copy_then_paste' | 'paste_as_keystrokes';
  ignore_ghost_clips: boolean;
  skip_sensitive?: boolean;
  skip_likely_secrets?: boolean;
  has_completed_onboarding?: boolean;
}

export interface PasteContext {
  target_kind: 'standard' | 'remote' | 'ninja';
  remote_paste_mode: 'copy_then_paste' | 'paste_as_keystrokes';
}

export type ClipType = 'text' | 'image' | 'html' | 'rtf' | 'file' | 'files' | 'url';

export const CLIP_TYPE_LABELS: Record<ClipType, string> = {
  text: 'Text',
  image: 'Image',
  html: 'HTML',
  rtf: 'Rich Text',
  file: 'File',
  files: 'Files',
  url: 'URL',
};

export const CLIP_TYPE_ICONS: Record<ClipType, string> = {
  text: 'FileText',
  image: 'Image',
  html: 'Code',
  rtf: 'Type',
  file: 'File',
  files: 'File',
  url: 'Link',
};
