# ESP-IDF build adapter

IDF owns Kconfig, component discovery, compilation, linking and images. fbuild
owns package resolution, the environment build directory, logging, size reporting
and exporting the images used by the existing deployer. Only ESP32-S3 is enabled.

`packages.json` pins the coherent ESP-IDF 5.4 / GCC 14.2 set from
platformio/platform-espressif32 v6.10.0. Downloads use the existing registry and
mirror-aware package installer. Python build tools run in uv's cached environment.
