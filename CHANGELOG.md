# Changelog

All notable Cubby Clipboard changes are documented here. PastePaw entries below Cubby's first beta are retained as upstream history and attribution.

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

