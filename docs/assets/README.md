# Cubby launch assets

This folder contains the reusable OCR launch set. Every screenshot and error message is staged; no real clipboard history, account data, notifications, or tray icons appear in these files.

## Primary files

| File | Format | Intended use |
| --- | --- | --- |
| `cubby-ocr-demo-1080p.mp4` | H.264, 1920×1080, 30 fps, 15 seconds | Reddit, X, Facebook, YouTube, press outreach |
| `cubby-ocr-demo.gif` | 960×540, 12 fps | README, landing page, Product Hunt gallery |
| `cubby-ocr-micro-loop.mp4` | H.264, 960×540, 4.2 seconds | Social posts and embeds that support MP4 |
| `cubby-ocr-micro-loop.gif` | 720×405, 12 fps | Small animated previews and thumbnails |
| `og-image.png` | 1200×630 | `cubbyclip.com` Open Graph and social preview |
| `hero-ocr-search.png` | 1920×1080 | OCR feature still, Store and Product Hunt source |
| `still-screenshot-saved.png` | 1920×1080 | Opening / local capture still |
| `still-paste-payoff.png` | 1920×1080 | Paste-back payoff still |
| `still-folders.png` | 1920×1080 | Project folders workflow |
| `still-privacy-controls.png` | 1920×1080 | Local storage and privacy controls |
| `still-win-v-comparison.png` | 1920×1080 | Cubby beside the familiar Win+V surface |
| `comparison-honest.png` | 1920×1080 | Cubby, Win+V, Ditto, and Raycast comparison |

## Channel-ready sets

- `product-hunt/` contains a 1270×760 animated first slide, five still slides, and a 240×240 thumbnail.
- `store/` contains five 1920×1080 screenshots in recommended listing order.
- `../press-kit/` contains writer-ready copies of the video, GIF, three stills, icon files, description, and links.
- `comparison-sources.md` records the first-party evidence and wording rules behind the comparison graphic.

## Source files

- `../../design/assets/ocr-demo-scenes.html` is the editable OCR demo scene layout and copy source.
- `../../design/assets/launch-stills.html` is the editable folders, privacy, Win+V, and comparison scene source.
- `source/cubby-ocr-search-window.mp4` is the privacy-safe Cubby UI capture used in the master.
- The remaining `source/scene-*.png` files are full-resolution rendered scenes.

The master deliberately uses a fake `CUBBY-0X800401D0` error and the search phrase `clipboard service unavailable`. The result is marked as 14 days old to make the OCR retrieval story immediately legible.
