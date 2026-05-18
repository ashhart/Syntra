# Syntra docs site

The static tutorial / reference site for Syntra, generated with
**MkDocs + mkdocs-material**.

## Why MkDocs

MkDocs was picked over Docusaurus, Astro Starlight, and VitePress for
one reason: **dependency surface.** The full toolchain — `mkdocs` +
`mkdocs-material` and their transitive deps — is ~30 Python packages
totalling ~10 MB on disk. Docusaurus's React + Webpack tree is
hundreds of packages and hundreds of megabytes. For a documentation
site that needs a dark theme, search, syntax highlighting, and
GitHub Pages deploy, mkdocs-material covers all four with no
plugins, no JavaScript build step, and no npm.

Trade-offs accepted:

- No MDX. Pages are plain Markdown. (We use no React components in
  the docs anyway.)
- Slower live-reload than Vite-backed alternatives. Not relevant for
  a docs site updated at the pace this one is.
- Theme customization happens via CSS variables and partial overrides
  rather than React components. The accent colour and near-black
  background are configured in `docs/assets/syntra.css`.

The full content of the site is in `docs/` (Markdown). The navigation
tree is in `mkdocs.yml`.

## Build

```bash
./build.sh
```

That:

1. Creates a Python venv at `/tmp/syntra-site-venv` if one doesn't
   exist (override with `SYNTRA_DOCS_VENV`).
2. Installs `mkdocs` + `mkdocs-material` if needed.
3. Runs `mkdocs build --strict --clean`.
4. Emits the static site to `site/build/` (see `site_dir` in
   `mkdocs.yml`).

The `--strict` flag turns any warning into a build failure — broken
links, missing nav targets, unresolved references. This is what
catches stale internal links during page authoring.

## Develop

```bash
source /tmp/syntra-site-venv/bin/activate
mkdocs serve
```

Then open `http://localhost:8000`. Pages live-reload as you save.

## Deploy

For GitHub Pages:

```bash
source /tmp/syntra-site-venv/bin/activate
mkdocs gh-deploy --no-history
```

That pushes `site/build/` to the repository's `gh-pages` branch.

For any static host (S3, Cloudflare Pages, Netlify), serve the
contents of `site/build/` from the host's static-file root. There is
no server-side component.

## Page status as of this release

| Section | Status |
|---------|--------|
| Home / overview | full |
| Quickstart | full |
| Concepts (6 pages) | full |
| API reference | full |
| Domain packs (10 pages) | full |
| Cookbook | **stub** — placeholder + TODO list of planned recipes |
| Operations | **stub** — placeholder + TODO list of planned topics |
| Migration guides | **stub** — one paragraph per path + TODO list |

The three stub pages each begin with an explicit `!!! warning` admonition
so a reader can't miss the stub status. Recipes and runbook detail
will land in a follow-up.
