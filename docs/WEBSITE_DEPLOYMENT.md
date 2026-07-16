# Cubby website deployment

The static website lives in `product_pages` and is deployed from this repository with Cloudflare Pages Git integration.

## Cloudflare Pages project

1. In Cloudflare, open **Workers & Pages** and choose **Create application → Pages → Connect to Git**.
2. Authorize only `tsouth89/cubby-clipboard` when possible.
3. Use these build settings:
   - Production branch: `main`
   - Framework preset: None
   - Build command: `exit 0`
   - Build output directory: `product_pages`
4. Deploy once and verify the generated `*.pages.dev` URL.
5. Under **Custom domains**, add `cubbyclipboard.com`.
6. Configure `cubbyclip.com` as a permanent redirect to `https://cubbyclipboard.com` with a Cloudflare Bulk Redirect.

Cloudflare automatically creates production deployments from `main` and preview deployments from other branches and pull requests.

## Pre-publish check

- Confirm GitHub release links match the intended public release state.
- Review the early-release warning and privacy limitation.
- Check the landing page and policy pages at desktop and phone widths.
- Verify HTTPS, canonical URLs, custom-domain redirects, and footer links.
