# Contributing to Cubby Clipboard

Thanks for helping improve Cubby. Bug reports, documentation fixes, focused feature proposals, and code contributions are welcome.

## Before you start

- Cubby supports Windows 11 only. Do not add macOS or Linux product work.
- Search existing issues before opening a duplicate.
- For a larger change, open a feature request before investing significant time.
- Never include clipboard contents, signing keys, tokens, or other secrets in an issue, log, screenshot, or commit.

## Development setup

Install Node.js, pnpm, Rust with the MSVC toolchain, Visual Studio C++ build tools, and WebView2. Then run:

```powershell
pnpm install --frozen-lockfile
pnpm run build

Push-Location src-tauri
cargo test --locked
Pop-Location

pnpm tauri dev
```

## Pull requests

Keep each pull request focused. Explain the user-facing problem, the approach, and how you tested it. Include before-and-after screenshots for visual changes. Update documentation when behavior changes.

Before opening a pull request, run the relevant checks:

```powershell
pnpm run format:check
pnpm run release:check
pnpm run build
node .github/scripts/check-site.mjs
node --test product_pages/worker.test.mjs

Push-Location src-tauri
cargo fmt --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
Pop-Location
```

By contributing, you agree that your work is provided under the repository's GPL-3.0 license and that you have the right to submit it.

## Community

Be kind, specific, and constructive. See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Security vulnerabilities belong in a private [GitHub security advisory](https://github.com/tsouth89/cubby-clipboard/security/advisories/new), not a public issue.
