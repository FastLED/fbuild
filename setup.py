"""
Setup file.
"""

import os

from setuptools import setup

URL = "https://github.com/fastled/fbuild"
KEYWORDS = "embedded arduino platformio compiler toolchain firmware microcontroller URL-based"
HERE = os.path.dirname(os.path.abspath(__file__))



if __name__ == "__main__":
    setup(
        maintainer="Zachary Vorhies",
        keywords=KEYWORDS,
        url=URL,
        package_data={"": ["assets/example.txt"]},
        include_package_data=True)

