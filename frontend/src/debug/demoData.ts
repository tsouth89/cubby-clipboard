import { ClipboardItem } from '../types';

function generateImage(label: string, color1: string, color2: string): string {
  const canvas = document.createElement('canvas');
  canvas.width = 400;
  canvas.height = 240;
  const ctx = canvas.getContext('2d')!;

  const gradient = ctx.createLinearGradient(0, 0, 400, 240);
  gradient.addColorStop(0, color1);
  gradient.addColorStop(1, color2);
  ctx.fillStyle = gradient;
  ctx.fillRect(0, 0, 400, 240);

  ctx.fillStyle = 'rgba(255,255,255,0.15)';
  ctx.beginPath();
  ctx.arc(320, 60, 80, 0, Math.PI * 2);
  ctx.fill();

  ctx.fillStyle = 'rgba(255,255,255,0.9)';
  ctx.font = 'bold 18px system-ui, sans-serif';
  ctx.textAlign = 'center';
  ctx.fillText(label, 200, 125);

  return canvas.toDataURL('image/png');
}

export function generateDemoClips(): ClipboardItem[] {
  const now = new Date();
  const ago = (minutes: number) => new Date(now.getTime() - minutes * 60000).toISOString();

  return [
    {
      id: 'demo-1',
      clip_type: 'text',
      content:
        'Cubby is a fast, private clipboard history replacement built specifically for Windows 11.',
      preview:
        'Cubby is a fast, private clipboard history replacement built specifically for Windows 11.',
      folder_id: null,
      created_at: ago(1),
      source_app: 'chrome.exe',
      source_icon: null,
      metadata: null,
    },
    {
      id: 'demo-2',
      clip_type: 'image',
      content: generateImage('Cubby — Dark Theme', '#102844', '#147ee8'),
      preview: '',
      folder_id: null,
      created_at: ago(3),
      source_app: 'Figma.exe',
      source_icon: null,
      metadata: JSON.stringify({ size_bytes: 184320 }),
    },
    {
      id: 'demo-3',
      clip_type: 'image',
      content: generateImage('Cubby — Light Theme', '#dceef9', '#32aeb1'),
      preview: '',
      folder_id: null,
      created_at: ago(5),
      source_app: 'Figma.exe',
      source_icon: null,
      metadata: JSON.stringify({ size_bytes: 201480 }),
    },
    {
      id: 'demo-4',
      clip_type: 'text',
      content: `pnpm install\ncargo install tauri-cli\npnpm tauri dev`,
      preview: 'pnpm install\ncargo install tauri-cli\npnpm tauri dev',
      folder_id: null,
      created_at: ago(8),
      source_app: 'WindowsTerminal.exe',
      source_icon: null,
      metadata: null,
    },
    {
      id: 'demo-5',
      clip_type: 'text',
      content: `Win+V         Toggle Cubby\nCtrl+F        Focus search\nEscape        Close / clear search\nEnter         Paste selected\nDelete        Delete selected\nP             Pin / Unpin`,
      preview: 'Win+V  Toggle Cubby\nCtrl+F  Focus search...',
      folder_id: null,
      created_at: ago(12),
      source_app: 'Code.exe',
      source_icon: null,
      metadata: null,
    },
    {
      id: 'demo-6',
      clip_type: 'text',
      content: `pub fn animate_window_show(window: &tauri::WebviewWindow) {\n    if IS_ANIMATING\n        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)\n        .is_err()\n    {\n        return;\n    }\n    // ... slide up from bottom\n}`,
      preview: 'pub fn animate_window_show(window: &tauri::WebviewWindow) {',
      folder_id: null,
      created_at: ago(15),
      source_app: 'Code.exe',
      source_icon: null,
      metadata: null,
    },
    {
      id: 'demo-9',
      clip_type: 'text',
      content:
        'All clipboard data is stored locally in a SQLite database on your machine. Cubby does not upload your history to a server.',
      preview: 'All clipboard data is stored locally in a SQLite database on your machine...',
      folder_id: null,
      created_at: ago(30),
      source_app: 'chrome.exe',
      source_icon: null,
      metadata: null,
    },
    {
      id: 'demo-10',
      clip_type: 'text',
      content: `git tag v0.1.0-beta.1\ngit push origin refs/heads/main\ngit push origin refs/tags/v0.1.0-beta.1`,
      preview: 'git tag v0.1.0-beta.1\ngit push origin refs/heads/main',
      folder_id: null,
      created_at: ago(35),
      source_app: 'WindowsTerminal.exe',
      source_icon: null,
      metadata: null,
    },

    {
      id: 'demo-12',
      clip_type: 'text',
      content:
        'Hi team,\n\nPlease find the latest Cubby v0.1.0-beta.1 build attached. It includes the cursor-positioned flyout, reliable history search, remote-session workflows, and the new tray icon.\n\nThanks',
      preview: 'Hi team, Please find the latest Cubby v0.1.0-beta.1 build...',
      folder_id: null,
      created_at: ago(50),
      source_app: 'OUTLOOK.EXE',
      source_icon: null,
      metadata: null,
    },
    {
      id: 'demo-13',
      clip_type: 'text',
      content:
        'Cubby 是一款专为 Windows 11 打造的快速、私密剪贴板历史工具。所有历史记录都存储在本地。',
      preview: 'Cubby 是一款专为 Windows 11 打造的快速、私密剪贴板历史工具...',
      folder_id: null,
      created_at: ago(60),
      source_app: 'WeChat.exe',
      source_icon: null,
      metadata: null,
    },

    {
      id: 'demo-19',
      clip_type: 'image',
      content: generateImage('Settings UI Mockup', '#7c2d12', '#f97316'),
      preview: '',
      folder_id: null,
      created_at: ago(80),
      source_app: 'Figma.exe',
      source_icon: null,
      metadata: JSON.stringify({ size_bytes: 156800 }),
    },
    {
      id: 'demo-20',
      clip_type: 'image',
      content: generateImage('Multi-monitor Screenshot', '#134e4a', '#14b8a6'),
      preview: '',
      folder_id: null,
      created_at: ago(100),
      source_app: 'Snipaste.exe',
      source_icon: null,
      metadata: JSON.stringify({ size_bytes: 348160 }),
    },
    {
      id: 'demo-14',
      clip_type: 'text',
      content: `## Cubby\n\nA **reliable** clipboard history replacement for Windows 11.\n\n- Persistent, searchable history\n- Local storage with no account\n- Native Win+V workflow\n- Remote-session support`,
      preview: '## Cubby — A reliable clipboard history replacement...',
      folder_id: null,
      created_at: ago(75),
      source_app: 'Obsidian.exe',
      source_icon: null,
      metadata: null,
    },
    {
      id: 'demo-16',
      clip_type: 'image',
      content: generateImage('App Icon 512×512', '#064e3b', '#10b981'),
      preview: '',
      folder_id: null,
      created_at: ago(65),
      source_app: 'Photoshop.exe',
      source_icon: null,
      metadata: JSON.stringify({ size_bytes: 92160 }),
    },
  ].map((clip) => ({ ...clip, is_pinned: false }));
}
