# ESP-IDF build adapter

IDF owns Kconfig, component discovery, compilation, linking and images. fbuild
owns package resolution, the environment build directory, logging, size reporting
and exporting images. ESP32/S2/S3/C2/C3/C6/H2 retain IDF 5.4; C5/P4 use IDF 5.5.5.

`packages.json` pins coherent framework/compiler sets per target. Downloads use
the existing registry and mirror-aware installer. Python build tools use uv.
P4 board silicon variants become native Kconfig defaults; incompatible active
configuration is rejected before compilation. See `docs/espidf.md` for details.
