# Cubby website deployment

The website lives in `product_pages` and is deployed from this repository with Cloudflare Pages Git integration. Cloudflare Pages advanced mode runs `product_pages/_worker.js` in front of the static assets for first-party analytics and the private `/admin` dashboard.

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

## Analytics configuration

Configure these encrypted variables for the production Pages project:

```powershell
npx wrangler pages secret put ADMIN_TOKEN --project-name cubby-clipboard
npx wrangler pages secret put ANALYTICS_SALT --project-name cubby-clipboard
npx wrangler pages secret put POSTHOG_PUBLIC_KEY --project-name cubby-clipboard
npx wrangler pages secret put POSTHOG_QUERY_KEY --project-name cubby-clipboard
npx wrangler pages secret put POSTHOG_PROJECT_ID --project-name cubby-clipboard
npx wrangler pages secret put GITHUB_TOKEN --project-name cubby-clipboard
```

- Use the existing SouthForge PostHog project. Cubby data is isolated by `$host` and `source`, both set to `cubbyclipboard.com`.
- `POSTHOG_PUBLIC_KEY` is the project's capture key. `POSTHOG_QUERY_KEY` is a personal API key limited to query-read access.
- `GITHUB_TOKEN` should be a fine-grained token limited to this repository with Administration: Read access, which unlocks GitHub's rolling 14-day traffic metrics.
- Generate long random values for `ADMIN_TOKEN` and `ANALYTICS_SALT`. Enter secrets directly in Wrangler or Cloudflare, never in source control or an issue.

The dashboard is available at `https://cubbyclipboard.com/admin`. The public site loads no third-party analytics script and sets no analytics cookie. The Worker rotates pseudonymous visitor identifiers every 30 days and does not pass raw IP addresses or user-agent strings to PostHog. The Cubby desktop application contains no analytics.

## Pre-publish check

- Confirm GitHub release links match the intended public release state.
- Review the early-release warning and privacy limitation.
- Check the landing page and policy pages at desktop and phone widths.
- Confirm `/admin` requires the private token and its PostHog and GitHub panels load.
- Confirm a page view appears under `source = cubbyclipboard.com` without a third-party script or analytics cookie.
- Verify HTTPS, canonical URLs, custom-domain redirects, and footer links.
