# Publishing cubbyclipboard.com with GitHub Pages

The website is intentionally static. `.github/workflows/pages.yml` publishes `product_pages` without a build step.

## Repository setup

1. Open the repository’s **Settings → Pages**.
2. Set the publishing source to **GitHub Actions**.
3. Set the custom domain to `cubbyclipboard.com`.
4. Enable **Enforce HTTPS** after the DNS check succeeds.

The included workflow uploads only `product_pages` as the Pages artifact. Desktop-app files are not published.

## Domains

- Primary: `cubbyclipboard.com`
- Redirect: `cubbyclip.com` → `https://cubbyclipboard.com`

Keep the canonical metadata pointed at the primary domain. Configure the short domain as a permanent redirect rather than serving duplicate pages.

## Pre-publish check

- Confirm the GitHub release links match the intended release state.
- Review the early-release warning and privacy limitation.
- Check the landing page and policy pages at desktop and phone widths.
- Verify HTTPS, the custom domain, and all footer links.
