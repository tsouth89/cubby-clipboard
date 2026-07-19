# Changelog

All notable Cubby Clipboard changes are documented here. PastePaw entries below Cubby's first beta are retained as upstream history and attribution.

## v1.2.1

### Fixed
- Cubby now opens at the top of your history, showing your most recent copy, instead of wherever you had last scrolled

### Changed
- Lower peak memory use when recognizing text in very large screenshots

## v1.2.0

### Added
- Redesigned Settings, organized into General, Privacy, Folders, and About with cleaner grouped sections
- History retention: choose how long Cubby keeps your history, from a week up to forever, with a storage-used readout. Pinned items are always kept

### Changed
- Search and filters reset each time Cubby opens, so you always start on your full, most-recent history
- A screenshot you just copied is read first and updates live, so it becomes searchable and offers Paste text moments after you copy it
- Timestamps keep counting up while Cubby is open
- New app and tray icons

### Fixed
- Reusing a clip no longer relabels its source as "Cubby Clipboard" or resets its timestamp
- Closing Cubby dismisses the right-click menu and any open dialog, so it reopens on a clean list

## v1.1.1

### Added
- Portable version: a self-contained zip that keeps your history, settings, and images in a data folder next to the app, with nothing written to AppData or the Windows registry

## v1.1.0

### Added
- Copy or paste the text recognized inside a screenshot by right-clicking an image result or pressing Shift+Enter
- Search now finds words inside your screenshots and shows where they matched

### Changed
- Recognized screenshot text keeps its line breaks now, instead of running together
- Your existing screenshots are re-scanned in the background so their recognized text improves too
- Screenshot text recognition runs as a durable background task that resumes after a restart
- Faster clipboard search that keeps your unencrypted text out of the database

## v1.0.2

### Fixed
- Clicking the Cubby tray icon now opens the full window rising from the taskbar instead of a small list

## v1.0.1

### Fixed
- The welcome guide now shows Win+V, the default way to open Cubby, instead of the raw configured shortcut name
- When Cubby opens near the bottom of the screen it now shows a compact list above the cursor instead of a jarring full-height window

## v1.0.0

### Added
- First-run welcome guide so new users know how to open and use Cubby
- Automatic updates: Cubby checks for new versions and installs them for you
- Skips clipboard items that apps such as password managers mark as sensitive, with a setting to capture everything if you prefer
- Offline text recognition for copied screenshots, so images are searchable by the text inside them
- Import clipboard history and pinned items from Ditto
- The configured hotkey opens Cubby inside supported remote sessions when the remote client's keyboard forwarding is off

### Changed
- Windows installers are now digitally signed, so Windows recognizes Cubby's publisher

## v0.1.0-beta.2

### Added
- The configured Cubby hotkey now opens the flyout inside supported remote sessions (Ninja Remote, Windows Remote Desktop, and other recognized clients) when the remote client's own keyboard forwarding is turned off, using the same low-level path as the `Win+V` replacement

### Changed
- Replaced the double-Ctrl remote-session gesture with the configured hotkey, which also avoids triggering Windows' "show pointer location on Ctrl" behavior

### Known limitations
- When a remote client such as Ninja Remote has keyboard forwarding enabled it captures the keyboard before Cubby can, so no hotkey opens Cubby in that mode; open Cubby from the tray icon instead

### 新增
- 当受支持的远程会话（Ninja Remote、Windows 远程桌面及其他已识别的客户端）关闭其键盘转发时，您配置的快捷键现在可在会话内打开 Cubby，使用与 `Win+V` 替代方案相同的底层机制

### 变更
- 以配置的快捷键取代了原有的双击 Ctrl 远程手势，同时避免触发 Windows 的“按 Ctrl 键时显示指针位置”行为

### 已知限制
- 当远程客户端（如 Ninja Remote）启用键盘转发时，它会先于 Cubby 捕获键盘，因此在该模式下没有任何快捷键能打开 Cubby；请改用托盘图标

## v0.1.0-beta.1

### Added
- Native Windows clipboard capture with contention retries and burst handling
- Cursor-anchored Windows 11 flyout with system accent, Mica/Acrylic effects, and automatic dismissal
- Optional native `Win+V` replacement without requiring PowerToys or another remapping tool
- Keyboard-first search, navigation, paste, plain-text paste, deletion, and persistent pinning
- Reliable local and remote-desktop workflows, including a large-text clipboard-sync mode
- Text, image, and file history with source application context, previews, content filters, folders, and contextual empty/error states
- Native-style clear-unpinned and explicit clear-everything actions
- Preservation of HTML, rich text, file lists, and other core Windows clipboard formats when replaying history
- Local history encryption using Windows-protected keys, authenticated encryption, and keyed content hashes

### Privacy and release notes
- Clipboard history remains local and Cubby includes no telemetry or cloud AI integrations
- This first release is unsigned; Windows SmartScreen may warn until signing and reputation are established
- Winget and Microsoft Store publishing are intentionally deferred until Cubby's package identities are reserved

### 新增
- 使用原生 Windows 剪贴板通知、竞争重试与突发复制处理
- 跟随鼠标位置的 Windows 11 弹出窗口，支持系统强调色、Mica/Acrylic 与自动关闭
- 可选的原生 `Win+V` 替代方案，无需 PowerToys 或其他按键映射工具
- 键盘优先的搜索、导航、粘贴、纯文本粘贴、删除与持久固定
- 本地及远程桌面可靠工作流，包括适合大段文本的剪贴板同步模式
- 文本、图片与文件历史、来源应用信息、预览、内容筛选、文件夹及清晰的空状态和错误状态
- 类原生的“清除未固定项目”与明确的“清除全部”操作
- 重放历史记录时保留 HTML、富文本、文件列表及其他核心 Windows 剪贴板格式
- 使用 Windows 保护密钥、认证加密与带密钥内容哈希对本地历史记录进行加密

### 隐私与发布说明
- 剪贴板历史仅保存在本地，Cubby 不包含遥测或云端 AI 集成
- 首个版本暂未签名，在建立签名与信誉之前 Windows SmartScreen 可能显示警告
- 在 Cubby 软件包标识完成预留之前，暂不发布至 Winget 与 Microsoft Store

## v1.3.7

### Added
- German, French, and Japanese language support

### Improved
- Winget release pipeline: hash verification step added before publishing to winget-pkgs to prevent stale-hash mismatches; release tag now explicitly pinned

### 新增
- 新增德语、法语、日语语言支持

### 优化
- Winget 发布流程：在发布至 winget-pkgs 前增加哈希值校验步骤，防止哈希不匹配问题；发布时明确指定 release tag

## v1.3.6

### Added
- Support floating window above the taskbar (toggle in Settings)
- Every release is now automatically scanned with VirusTotal (70+ antivirus engines) — scan results are linked in the release notes

### 新增
- 窗口支持浮动在任务栏上层（可在设置中开启/关闭）
- 每次发布版本现在会自动通过 VirusTotal（70+ 款杀毒引擎）进行安全扫描，扫描结果链接附在 Release 说明中

## v1.3.5

### Added
- Native rounded corners support for all window effects (Mica, Mica Alt, Clear) using Windows 11 DWM — toggle on/off in Settings

### Fixed
- Fixed TypeScript build error caused by missing Vite client types (`import.meta.env`)

### 新增
- 所有窗口效果（Mica、Mica Alt、Clear）均支持原生圆角，通过 Windows 11 DWM 实现，可在设置中开启/关闭

### 修复
- 修复因缺少 Vite 客户端类型导致的 TypeScript 构建错误（`import.meta.env`）

## v1.3.4

### Added
- Brand new native style look with Windows Mica and Mica-Alt window effects for a seamless, beautiful appearance that blends with your desktop

### 新增
- 全新原生风格外观，支持 Windows Mica 和 Mica-Alt 窗口效果，与桌面完美融合，带来更精美的视觉体验

## v1.3.3

### Changed
- Refined UI layout: reduced window height, tightened card spacing, fixed control bar height, and removed CSS shadow in Clear window effect mode

### 变更
- 优化界面布局：减小窗口高度、收紧卡片间距、固定控制栏高度，并在"无效果"窗口模式下移除 CSS 阴影

## v1.3.2

### Fixed
- Fixed hotkey toggle broken after changing hotkey in settings (issue #6)
- Fixed winget package missing arm64 installer by switching to NSIS setup.exe for architecture detection (issue #7)

## v1.3.1

### Fixed
- Removed white/alpha border around settings window in dark mode

