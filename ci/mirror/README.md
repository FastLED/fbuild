# ci/mirror

The tool that fills fbuild's download mirror bucket (`fbuild`, DigitalOcean
Spaces `sfo3`) from the PlatformIO registry. It moved here from an unversioned
local directory (FastLED/fbuild#2040).

- **`mirror.py`** -- Catalogs the PlatformIO registry into a SQLite state DB
  and transfers every pending file to the bucket. Runs on a droplet.
- **`rotate.py`** -- Local supervisor that runs `mirror.py` on rotating
  droplets to work around the registry's per-IP daily download cap.
