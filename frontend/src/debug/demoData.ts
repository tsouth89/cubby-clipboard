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
        'PastePaw is a beautiful clipboard history manager for Windows, built with Rust + Tauri + React + TypeScript.',
      preview:
        'PastePaw is a beautiful clipboard history manager for Windows, built with Rust + Tauri + React + TypeScript.',
      folder_id: null,
      created_at: ago(1),
      source_app: 'chrome.exe',
      source_icon: null,
      metadata: null,
    },
    {
      id: 'demo-2',
      clip_type: 'image',
      content: generateImage('PastePaw — Dark Theme', '#1e1b4b', '#4c1d95'),
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
      content: generateImage('PastePaw — Light Theme', '#dbeafe', '#6366f1'),
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
      content: `Win+Alt+V     Toggle window\nCtrl+F        Focus search\nEscape        Close / clear search\nEnter         Paste selected\nDelete        Delete selected\nP             Pin / Unpin`,
      preview: 'Win+Alt+V  Toggle window\nCtrl+F  Focus search...',
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
        'All clipboard data is stored locally in a SQLite database on your machine. PastePaw never uploads your data to any server.',
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
      content: `git tag v1.3.3\ngit push origin refs/heads/main\ngit push origin refs/tags/v1.3.3`,
      preview: 'git tag v1.3.3\ngit push origin refs/heads/main',
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
        'Hi team,\n\nPlease find the latest build of PastePaw v1.3.3 attached. Key changes include refined UI layout, tighter card spacing, and a new dark-mode tray icon.\n\nBest,\nXueshi',
      preview: 'Hi team, Please find the latest build of PastePaw v1.3.3...',
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
        'PastePaw 是一款为 Windows 打造的精美剪贴板历史管理工具，基于 Rust + Tauri + React + TypeScript 构建。所有数据仅存储在本地，绝不上传。',
      preview: 'PastePaw 是一款为 Windows 打造的精美剪贴板历史管理工具...',
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
      content: `## PastePaw\n\nA **beautiful** clipboard history manager for Windows.\n\n- Built with Rust + Tauri\n- 100% local storage\n- AI powered actions\n\n> "The best clipboard manager for Windows" `,
      preview: '## PastePaw — A beautiful clipboard history manager...',
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
  ];
}
