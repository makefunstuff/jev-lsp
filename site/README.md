# site

The documentation site for jev-lsp: what the server is, how a rules pass works, what it costs,
how to install it for three clients, and what is not proven. One page, one stylesheet, no script.

```
site/
  index.html      the page
  styles.css      the stylesheet
  assets/         three copies of docs/assets/ (see below)
  .nojekyll       so a file beginning with `_` is served rather than skipped
  README.md       this file
```

## Preview

From this directory, not from the repository root:

```sh
cd site
python3 -m http.server 8000
# then read http://127.0.0.1:8000/
```

Every path in `index.html` is relative to the site root (`./styles.css`, `./assets/…`), so
`http://127.0.0.1:8000/` from inside `site/` resolves the same way GitHub Pages will.

## Deploy

`site/` is published as the document root of its own site, so nothing outside `site/` is
reachable from a published page. A person flips these settings in **Settings → Pages**; the
repository's `.github/` is owned elsewhere and no workflow is created here.

GitHub Pages offers two sources, and only the second can serve `site/`:

1. **Deploy from a branch** takes a branch and then one folder, and the folder list is
   `/ (root)` or `/docs`. It cannot point at `site/`. To use this route, mirror the contents of
   `site/` to the root of a `gh-pages` branch, then set Source = *Deploy from a branch*,
   Branch = `gh-pages`, Folder = `/ (root)`.
2. **GitHub Actions** serves any directory, and is the route that publishes `site/` in place.
   A workflow at `.github/workflows/pages.yml` with `actions/configure-pages`,
   `actions/upload-pages-artifact` over `path: site`, and `actions/deploy-pages` does it; then set
   Source = *GitHub Actions*. The job needs `permissions: pages: write, id-token: write` and a
   `github-pages` environment.

Either way the project page lands at `https://makefunstuff.github.io/jev-lsp/`, which is a
subpath rather than a domain root. That is why every link and every asset reference in
`index.html` is relative: an absolute `/styles.css` would resolve to
`https://makefunstuff.github.io/styles.css` and 404.

## The images

`assets/jev-steering.svg`, `assets/jev-neovim.webp` and `assets/jev-cursor.webp` are **copies** of
`docs/assets/jev-steering.svg`, `docs/assets/jev-neovim.webp` and `docs/assets/jev-cursor.webp`,
under the same names. The site is served as its own root, so a path to `../docs/assets/` would
404 in production and still work in a local preview, which is the failure mode this avoids.
One copy, one direction: re-syncing is `cp docs/assets/{jev-steering.svg,jev-neovim.webp,jev-cursor.webp} site/assets/`.

## Where the page's content comes from

Every section quotes one of these documents. When one changes, the quoting is what to check.

**One place the page does not follow a document.** `README.md` still says there is no release
page yet, and my copy of that sentence was wrong, so the install section now names the
[v0.1.0 release](https://github.com/makefunstuff/jev-lsp/releases/tag/v0.1.0) and its assets
instead. The release exists, is not a draft and is not a prerelease, and its assets are
`jev-0.1.0.vsix`, three `jev-lsp-v0.1.0-<target>.tar.gz` archives and `SHA256SUMS`; that list came
from `https://api.github.com/repos/makefunstuff/jev-lsp/releases/tags/v0.1.0`. When `README.md`
catches up, the two can be re-synced.

| Section of the page | Source |
|---|---|
| The opening statement, what it is not, the classifier framing | `README.md` `## What it is for` |
| The four-step loop, the screenshots' captions | `README.md` `## What it is for`, `docs/TUTORIAL.md` §2 |
| The cost tables and the token rates | `README.md` `## What it costs`, `docs/MODEL.md` §7 (measured) and §8 (local) |
| The three verbatim rule documents | `.jev/rules/words-must-carry-a-fact.json`, `.jev/rules/no-unwrap-outside-tests.json`, `.jev/rules/no-swallowed-failures.json` |
| The use-case measurements | `docs/TUTORIAL.md` §2.1–§2.4 |
| The rule fields, the two rules of thumb, the twenty-one-rule inventory | `docs/GUIDE.md` §4 and `.jev/rules/*.json` |
| Install for each client, the keys, the endpoints | `README.md` `## Use it`, `### Endpoints`, `## What you get`, `## Use it with another LSP client`; `docs/TUTORIAL.md` §1; `docs/CURSOR.md` |
| The release, the repository and the licence links | `Cargo.toml` (`repository`, `license = "MIT"`, `license-file = "LICENSE"`), `README.md` `## Licence`, `STATUS.md` (the dated decision), and the release's own asset list |
| The recorded OMP transcript and the finding id | `docs/assets/jev-steering.svg`, from the fixture in `verify/omp_lsp.sh` |
| The verification table and the negative controls | `docs/VERIFICATION.md`, the artefact table and §8 |
| The limitations | `docs/VERIFICATION.md` §10 and §11, `docs/MODEL.md` §8 |
| The register of the page's own prose | `docs/STYLE.md` |
| The rendering of a code block, a panel and a capture | `docs/assets/jev-steering.svg` (its palette and framing) |

## What the stylesheet does, and does not

- No framework, no webfont, no `@import`, no `url()`, no request to any host but the site's own.
  The stacks are system stacks: a serif for prose, a sans for headings and labels, monospace for
  code.
- Four type sizes plus monospace, one measure of 70 characters for prose, and one accent: the
  amber `#f0b849` from the highlighted diagnostic row in `docs/assets/jev-steering.svg`, with a
  darker tone of the same hue for text on paper.
- One breakpoint at 48rem. Below it a label-and-prose table stacks into a definition list rather
  than scrolling sideways; the tabular tables keep their scroll box.
- A dark variant exists under `prefers-color-scheme: dark`, from the same variables.
- A print block turns the code panels light and hides the navigation. It was checked by
  rendering the page with that block applied to screen media; page breaks inside a printed table
  are not tested.
- The visuals were checked by rendering through `qlmanage -t -s 1400` and reading the PNGs, with
  each section in a 1024px frame and in a 380px frame, because a Quick Look thumbnail is a square
  crop of the top of a page and a page is taller than its thumbnail.
