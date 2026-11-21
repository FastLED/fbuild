# ESP-IDF component project root for the RISC-V targets (FastLED/fbuild#1878).
# fbuild delegates to idf.py, which includes this file unchanged.
cmake_minimum_required(VERSION 3.16)

include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(riscv_component_demo)
