"""Unit tests for CompileDatabase and ClangFlagTranslator."""

import json
import threading
from pathlib import Path

from fbuild.build.clang_flag_translator import ClangFlagTranslator
from fbuild.build.compile_database import CompileDatabase


class TestCompileDatabase:
    """Tests for CompileDatabase collector."""

    def test_empty_database(self) -> None:
        db = CompileDatabase()
        assert not db.has_entries()
        assert db.entry_count() == 0
        assert db.to_json() == "[]\n"

    def test_add_entry(self) -> None:
        db = CompileDatabase()
        db.add_entry(
            directory="/project",
            file="/project/src/main.cpp",
            arguments=["g++", "-c", "src/main.cpp", "-o", "main.o"],
            output="main.o",
        )
        assert db.has_entries()
        assert db.entry_count() == 1

    def test_to_json_format(self) -> None:
        db = CompileDatabase()
        db.add_entry(
            directory="/project",
            file="/project/src/main.cpp",
            arguments=["g++", "-O2", "-c", "src/main.cpp", "-o", "main.o"],
            output="main.o",
        )
        result = json.loads(db.to_json())
        assert len(result) == 1
        entry = result[0]
        assert entry["directory"] == "/project"
        assert entry["file"] == "/project/src/main.cpp"
        assert entry["arguments"] == ["g++", "-O2", "-c", "src/main.cpp", "-o", "main.o"]
        assert entry["output"] == "main.o"

    def test_write_and_load(self, tmp_path: Path) -> None:
        db = CompileDatabase()
        db.add_entry(
            directory="/project",
            file="/project/src/main.cpp",
            arguments=["g++", "-c", "main.cpp"],
            output="main.o",
        )
        db.add_entry(
            directory="/project",
            file="/project/src/util.c",
            arguments=["gcc", "-c", "util.c"],
            output="util.o",
        )

        out_path = tmp_path / "compile_commands.json"
        db.write(out_path)
        assert out_path.exists()

        loaded = CompileDatabase.load(out_path)
        assert loaded.entry_count() == 2

        entries = loaded.get_entries()
        assert entries[0].file == "/project/src/main.cpp"
        assert entries[1].file == "/project/src/util.c"

    def test_load_nonexistent(self, tmp_path: Path) -> None:
        db = CompileDatabase.load(tmp_path / "nonexistent.json")
        assert db.entry_count() == 0

    def test_merge_no_duplicates(self) -> None:
        db1 = CompileDatabase()
        db1.add_entry(directory="/p", file="a.cpp", arguments=["g++", "a.cpp"], output="a.o")

        db2 = CompileDatabase()
        db2.add_entry(directory="/p", file="b.cpp", arguments=["g++", "b.cpp"], output="b.o")

        db1.merge(db2)
        assert db1.entry_count() == 2

    def test_merge_with_duplicates(self) -> None:
        db1 = CompileDatabase()
        db1.add_entry(directory="/p", file="a.cpp", arguments=["g++", "-O0", "a.cpp"], output="a.o")

        db2 = CompileDatabase()
        db2.add_entry(directory="/p", file="a.cpp", arguments=["g++", "-O2", "a.cpp"], output="a.o")

        db1.merge(db2)
        assert db1.entry_count() == 1
        # Latest wins
        assert db1.get_entries()[0].arguments == ["g++", "-O2", "a.cpp"]

    def test_strip_sccache(self) -> None:
        cmd = ["/usr/bin/sccache", "g++", "-c", "main.cpp"]
        result = CompileDatabase.strip_sccache(cmd)
        assert result == ["g++", "-c", "main.cpp"]

    def test_strip_sccache_windows(self) -> None:
        cmd = ["C:\\tools\\sccache.exe", "g++", "-c", "main.cpp"]
        result = CompileDatabase.strip_sccache(cmd)
        assert result == ["g++", "-c", "main.cpp"]

    def test_strip_sccache_no_sccache(self) -> None:
        cmd = ["g++", "-c", "main.cpp"]
        result = CompileDatabase.strip_sccache(cmd)
        assert result == ["g++", "-c", "main.cpp"]

    def test_strip_sccache_empty(self) -> None:
        assert CompileDatabase.strip_sccache([]) == []

    def test_thread_safety(self) -> None:
        db = CompileDatabase()
        errors: list[Exception] = []

        def add_entries(start: int, count: int) -> None:
            try:
                for i in range(start, start + count):
                    db.add_entry(
                        directory="/project",
                        file=f"/project/src/file_{i}.cpp",
                        arguments=["g++", "-c", f"file_{i}.cpp"],
                        output=f"file_{i}.o",
                    )
            except Exception as e:
                errors.append(e)

        threads = [threading.Thread(target=add_entries, args=(i * 100, 100)) for i in range(4)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        assert len(errors) == 0
        assert db.entry_count() == 400


class TestClangFlagTranslator:
    """Tests for GCC-to-clang flag translation."""

    def test_xtensa_translation(self) -> None:
        flags = [
            "xtensa-esp-elf-g++",
            "-mlongcalls",
            "-mdisable-hardware-atomics",
            "-O2",
            "-std=gnu++17",
            "-c",
            "main.cpp",
        ]
        result = ClangFlagTranslator.translate(flags, "xtensa", "esp32")
        assert result[0] == "clang++"
        assert "--target=xtensa-esp-elf" in result
        assert "-mlongcalls" not in result
        assert "-mdisable-hardware-atomics" not in result
        assert "-O2" in result
        assert "-std=gnu++17" in result

    def test_avr_translation(self) -> None:
        flags = [
            "avr-gcc",
            "-mmcu=atmega328p",
            "-O2",
            "-flto",
            "-c",
            "main.c",
        ]
        result = ClangFlagTranslator.translate(flags, "avr", "atmega328p")
        assert result[0] == "clang"
        assert "--target=avr" in result
        assert "-mmcu=atmega328p" in result
        assert "-flto" not in result

    def test_riscv_translation(self) -> None:
        flags = [
            "riscv32-esp-elf-g++",
            "-march=rv32imac_zicsr_zifencei",
            "-mabi=ilp32",
            "-c",
            "main.cpp",
        ]
        result = ClangFlagTranslator.translate(flags, "riscv32", "esp32c6")
        assert result[0] == "clang++"
        assert "--target=riscv32-esp-elf" in result
        assert "-march=rv32imac_zicsr_zifencei" in result
        assert "-mabi=ilp32" not in result

    def test_arm_translation(self) -> None:
        flags = [
            "arm-none-eabi-g++",
            "-mcpu=cortex-m4",
            "-mthumb-interwork",
            "-O2",
            "-c",
            "main.cpp",
        ]
        result = ClangFlagTranslator.translate(flags, "arm", "stm32f407vg")
        assert result[0] == "clang++"
        assert "--target=arm-none-eabi" in result
        assert "-mcpu=cortex-m4" in result
        assert "-mthumb-interwork" not in result

    def test_lto_flags_removed(self) -> None:
        flags = [
            "g++",
            "-flto=auto",
            "-fno-fat-lto-objects",
            "-fuse-linker-plugin",
            "-c",
            "main.cpp",
        ]
        result = ClangFlagTranslator.translate(flags, "arm", "stm32f407vg")
        assert "-flto=auto" not in result
        assert "-fno-fat-lto-objects" not in result
        assert "-fuse-linker-plugin" not in result

    def test_gcc_compiler_becomes_clang(self) -> None:
        result = ClangFlagTranslator.translate(["avr-gcc", "-c", "test.c"], "avr", "atmega328p")
        assert result[0] == "clang"

    def test_gxx_compiler_becomes_clangxx(self) -> None:
        result = ClangFlagTranslator.translate(["avr-g++", "-c", "test.cpp"], "avr", "atmega328p")
        assert result[0] == "clang++"

    def test_empty_flags(self) -> None:
        assert ClangFlagTranslator.translate([], "avr", "atmega328p") == []

    def test_get_target_triple(self) -> None:
        assert ClangFlagTranslator.get_target_triple("xtensa", "esp32") == "xtensa-esp-elf"
        assert ClangFlagTranslator.get_target_triple("riscv32", "esp32c6") == "riscv32-esp-elf"
        assert ClangFlagTranslator.get_target_triple("avr", "atmega328p") == "avr"
        assert ClangFlagTranslator.get_target_triple("arm", "stm32f407vg") == "arm-none-eabi"
        assert ClangFlagTranslator.get_target_triple("unknown", "x") is None
