# Nib landing page → `nib.pages.dev`

A self-contained static site for Nib. **No build step, no dependencies, no
JavaScript, no third-party requests** — just `index.html` + `styles.css` +
`favicon.svg`, served as-is. This matches Nib's own "vanilla, no-tracking"
ethos and lets the Content-Security-Policy (`_headers`) stay strict.

```
site/
├── index.html     # the page
├── styles.css     # all styling (no inline styles, so CSP needs no 'unsafe-inline')
├── favicon.svg
├── _headers       # Cloudflare Pages security headers (strict CSP)
└── README.md      # this file
```

Preview locally with any static server, e.g.:

```bash
python3 -m http.server -d site 8000   # then open http://localhost:8000
```

## Deploy to Cloudflare Pages

The `nib.pages.dev` subdomain is assigned automatically when the Pages
**project name** is `nib` (assuming it's free in your account). Two ways:

### A. Git integration (recommended — auto-deploys on push)

1. Cloudflare dashboard → **Workers & Pages** → **Create** → **Pages** →
   **Connect to Git**.
2. Pick this repo. Set **Project name** = `nib` → you get `https://nib.pages.dev`.
3. Build settings:
   - **Framework preset:** None
   - **Build command:** *(leave empty)*
   - **Build output directory:** `site`
4. **Save and Deploy.** Every push to the production branch redeploys.

### B. Wrangler CLI (one-off / scripted)

```bash
npm i -g wrangler
wrangler login                         # or set CLOUDFLARE_API_TOKEN
wrangler pages project create nib      # claims nib.pages.dev
wrangler pages deploy site --project-name nib
```

## After it's live

- Set the GitHub repo **Website** field (About → gear) to `https://nib.pages.dev`.
- A custom domain (e.g. `nib.app`) can be attached later under the project's
  **Custom domains** tab.

> The outbound GitHub links in `index.html` point at `github.com/abgnydn/quill`
> so they work today. GitHub auto-redirects after the repo rename, so they keep
> working as `…/nib`; flip them to `/nib` whenever you like.
