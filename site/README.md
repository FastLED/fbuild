# site

Static files published on fbuild's GitHub Pages site
(<https://fastled.github.io/fbuild/>) alongside the benchmark report.
`benchmark-build-comparison.yml` copies them in on every Pages deploy.

The deploy is independent of the benchmark measurement (FastLED/fbuild#1825):
the publish job runs once the benchmark job has finished, seeds the site from
the last published `benchmark-stats` branch, and overlays fresh measurements
only when they exist. A broken or timed-out benchmark therefore keeps the
previous report online while these files still reach the site.

- `mirror/` — server-side download fallback settings (FastLED/fbuild#1810).
