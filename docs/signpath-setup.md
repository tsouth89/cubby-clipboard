# SignPath Code Signing Setup

PastePaw uses [SignPath Foundation](https://signpath.org/) for free code signing (open source program).
A valid Authenticode signature eliminates false positive AV detections from ML-based engines.

## Form Fields (OSSRequestForm-v4.xlsx)

| Field | Value |
|-------|-------|
| Name | `PastePaw` |
| Handle | `PastePaw` |
| Type | `Program` |
| License | `GPL-3.0` — https://opensource.org/licenses/GPL-3.0 |
| Repository URL | https://github.com/XueshiQiao/PastePaw |
| Homepage URL | https://github.com/XueshiQiao/PastePaw |
| Download URL | https://github.com/XueshiQiao/PastePaw/releases |
| Privacy Policy URL | *(blank — no user data collected or transmitted)* |
| Wikipedia URL | *(blank)* |
| Tagline | `Free, open-source clipboard history manager for Windows` |
| Description | `PastePaw is a lightweight, privacy-first clipboard history manager for Windows, built with Rust and Tauri. All data is stored locally with no telemetry or cloud sync.` |
| Reputation | Available on winget (`winget install XueshiQiao.PastePaw`), which requires passing Microsoft's official validation pipeline including antivirus scanning and human moderation review. GitHub releases: https://github.com/XueshiQiao/PastePaw/releases |
| User Full Name | *(your real name)* |
| User Email | *(your email)* |
| Build System | `GitHub Actions` |
| Accept terms | `I hereby accept the terms of use` |

## Post-Approval Setup

### 1. SignPath.io Configuration
- Log in at https://signpath.io with your GitHub account
- Create a **Project**: `PastePaw`
- Create an **Artifact Configuration**:
  - Type: `pe` (Portable Executable)
  - File pattern: `*-setup.exe`
- Create a **Signing Policy**: `release-signing`
- Copy your **Organization ID** from Settings

### 2. GitHub Secrets
Add to repo secrets (`Settings → Secrets → Actions`):
- `SIGNPATH_API_TOKEN` — from SignPath.io → Settings → API Tokens
- `SIGNPATH_ORG_ID` — from SignPath.io → Settings → Organization

### 3. Pipeline Integration
Update `release.yml` to:
1. Upload the built installer as a GitHub Actions artifact
2. Submit to SignPath for signing via `SignPath/github-actions`
3. Download the signed installer
4. Upload the signed installer to the GitHub release

See `release.yml` for the actual implementation once credentials are available.

## References
- [SignPath Foundation (OSS program)](https://signpath.org/)
- [SignPath GitHub Actions](https://github.com/SignPath/github-actions)
- [DB Browser for SQLite's experience with SignPath](https://sqlitebrowser.org/blog/signing-windows-executables-our-journey-with-signpath/)
