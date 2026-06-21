# www_static

Source-of-truth copy of the static assets published to the orphan `www`
branch by the daily `update-data.yml` workflow. Files here are mirrored
verbatim onto `www/` before the day's SQLite database is built.

| File           | Purpose                                                   |
| -------------- | --------------------------------------------------------- |
| `index.html`   | The query UI (input fields, canned-query buttons, table). |
| `app.js`       | sql.js loader, fuzzy ranker, parameter binding.           |
| `style.css`    | Minimal styling — readable on phones, no framework.       |

`sql-wasm.js` / `sql-wasm.wasm` are NOT vendored here. The workflow
downloads a pinned release with an SRI hash check and stages them onto
the `www` branch — see `.github/workflows/update-data.yml`.

To preview locally:

```bash
# After running update-data.yml once, the www branch will exist.
git worktree add /tmp/www www
cd /tmp/www
python3 -m http.server 8000
# Open http://localhost:8000/
```

The committed UI on `www` always matches `online-data-tools/www_static/`
on `main` — the workflow overwrites it on every nightly run.
