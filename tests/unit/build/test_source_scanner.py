"""Tests for source file scanner and .ino preprocessor."""

import pytest
from pathlib import Path
from zapio.build.source_scanner import SourceScanner, SourceCollection


class TestSourceScanner:
    """Test source file scanning and preprocessing."""

    @pytest.fixture
    def temp_project(self, tmp_path):
        """Create temporary project structure."""
        project = tmp_path / "test_project"
        project.mkdir()

        src = project / "src"
        src.mkdir()

        build = project / ".zap" / "build" / "uno"
        build.mkdir(parents=True)

        return {
            'project': project,
            'src': src,
            'build': build
        }

    def test_init(self, temp_project):
        """Test scanner initialization."""
        scanner = SourceScanner(
            temp_project['project'],
            temp_project['build']
        )
        assert scanner.project_dir == temp_project['project']
        assert scanner.build_dir == temp_project['build']

    def test_scan_empty_directory(self, temp_project):
        """Test scanning empty source directory."""
        scanner = SourceScanner(
            temp_project['project'],
            temp_project['build']
        )
        result = scanner.scan()

        assert isinstance(result, SourceCollection)
        assert len(result.sketch_sources) == 0
        assert len(result.core_sources) == 0
        assert len(result.variant_sources) == 0

    def test_scan_single_ino_file(self, temp_project):
        """Test scanning single .ino file."""
        # Create .ino file
        ino_file = temp_project['src'] / "sketch.ino"
        ino_file.write_text("""
void setup() {
  pinMode(13, OUTPUT);
}

void loop() {
  digitalWrite(13, HIGH);
}
""")

        scanner = SourceScanner(
            temp_project['project'],
            temp_project['build']
        )
        result = scanner.scan()

        assert len(result.sketch_sources) == 1
        # Check that .cpp file was generated
        cpp_file = result.sketch_sources[0]
        assert cpp_file.exists()
        assert cpp_file.suffix == '.cpp'

        # Check content
        content = cpp_file.read_text()
        assert '#include <Arduino.h>' in content
        assert 'void setup();' in content
        assert 'void loop();' in content

    def test_scan_multiple_ino_files(self, temp_project):
        """Test scanning multiple .ino files (should concatenate)."""
        # Create multiple .ino files
        (temp_project['src'] / "a_main.ino").write_text("void setup() {}")
        (temp_project['src'] / "b_helpers.ino").write_text("void helper() {}")

        scanner = SourceScanner(
            temp_project['project'],
            temp_project['build']
        )
        result = scanner.scan()

        assert len(result.sketch_sources) == 1
        cpp_file = result.sketch_sources[0]
        content = cpp_file.read_text()

        # Both functions should be present
        assert 'void setup() {}' in content
        assert 'void helper() {}' in content

    def test_scan_cpp_files(self, temp_project):
        """Test scanning existing .cpp files."""
        cpp_file = temp_project['src'] / "main.cpp"
        cpp_file.write_text("int main() { return 0; }")

        scanner = SourceScanner(
            temp_project['project'],
            temp_project['build']
        )
        result = scanner.scan()

        assert len(result.sketch_sources) == 1
        assert result.sketch_sources[0] == cpp_file

    def test_scan_c_files(self, temp_project):
        """Test scanning .c files."""
        c_file = temp_project['src'] / "utils.c"
        c_file.write_text("void util_func() {}")

        scanner = SourceScanner(
            temp_project['project'],
            temp_project['build']
        )
        result = scanner.scan()

        assert len(result.sketch_sources) == 1
        assert result.sketch_sources[0] == c_file

    def test_scan_mixed_sources(self, temp_project):
        """Test scanning mix of .ino, .cpp, and .c files."""
        (temp_project['src'] / "sketch.ino").write_text("void setup() {}")
        (temp_project['src'] / "helper.cpp").write_text("void help() {}")
        (temp_project['src'] / "util.c").write_text("void util() {}")

        scanner = SourceScanner(
            temp_project['project'],
            temp_project['build']
        )
        result = scanner.scan()

        # Should have 3 sources: generated .cpp, helper.cpp, util.c
        assert len(result.sketch_sources) == 3

    def test_scan_headers(self, temp_project):
        """Test finding header files."""
        (temp_project['src'] / "config.h").write_text("#define LED 13")
        (temp_project['src'] / "utils.hpp").write_text("void util();")

        scanner = SourceScanner(
            temp_project['project'],
            temp_project['build']
        )
        result = scanner.scan()

        assert len(result.headers) == 2

    def test_scan_subdirectories(self, temp_project):
        """Test scanning sources in subdirectories."""
        subdir = temp_project['src'] / "lib"
        subdir.mkdir()

        (subdir / "module.cpp").write_text("void module() {}")
        (subdir / "helper.c").write_text("void help() {}")

        scanner = SourceScanner(
            temp_project['project'],
            temp_project['build']
        )
        result = scanner.scan()

        assert len(result.sketch_sources) == 2

    def test_scan_core_sources(self, temp_project):
        """Test scanning Arduino core sources."""
        core_dir = temp_project['project'] / "core"
        core_dir.mkdir()

        (core_dir / "wiring.c").write_text("void init() {}")
        (core_dir / "main.cpp").write_text("int main() {}")

        scanner = SourceScanner(
            temp_project['project'],
            temp_project['build']
        )
        result = scanner.scan(core_dir=core_dir)

        assert len(result.core_sources) == 2

    def test_scan_variant_sources(self, temp_project):
        """Test scanning variant sources."""
        variant_dir = temp_project['project'] / "variant"
        variant_dir.mkdir()

        (variant_dir / "pins.cpp").write_text("const uint8_t pins[] = {}")

        scanner = SourceScanner(
            temp_project['project'],
            temp_project['build']
        )
        result = scanner.scan(variant_dir=variant_dir)

        assert len(result.variant_sources) == 1

    def test_preprocess_simple_ino(self, temp_project):
        """Test preprocessing simple .ino file."""
        ino_file = temp_project['src'] / "blink.ino"
        ino_file.write_text("""
int led = 13;

void setup() {
  pinMode(led, OUTPUT);
}

void loop() {
  digitalWrite(led, HIGH);
  delay(1000);
  digitalWrite(led, LOW);
  delay(1000);
}
""")

        scanner = SourceScanner(
            temp_project['project'],
            temp_project['build']
        )
        result = scanner.scan()

        cpp_file = result.sketch_sources[0]
        content = cpp_file.read_text()

        # Check Arduino.h included
        assert '#include <Arduino.h>' in content

        # Check prototypes generated
        assert 'void setup();' in content
        assert 'void loop();' in content

        # Check original code present
        assert 'int led = 13;' in content

    def test_preprocess_with_custom_functions(self, temp_project):
        """Test preprocessing with custom functions."""
        ino_file = temp_project['src'] / "sketch.ino"
        ino_file.write_text("""
void setup() {
  Serial.begin(9600);
}

void loop() {
  printMessage();
}

void printMessage() {
  Serial.println("Hello");
}

int calculate(int x, int y) {
  return x + y;
}
""")

        scanner = SourceScanner(
            temp_project['project'],
            temp_project['build']
        )
        result = scanner.scan()

        cpp_file = result.sketch_sources[0]
        content = cpp_file.read_text()

        # Check all prototypes
        assert 'void setup();' in content
        assert 'void loop();' in content
        assert 'void printMessage();' in content
        assert 'int calculate(int x, int y);' in content

    def test_preprocess_with_comments(self, temp_project):
        """Test preprocessing ignores comments."""
        ino_file = temp_project['src'] / "sketch.ino"
        ino_file.write_text("""
// This is a comment
void setup() {
  // Initialize
}

/* Multi-line
   comment */
void loop() {
  /* inline comment */ blink();
}

void blink() {
  // Blink LED
}
""")

        scanner = SourceScanner(
            temp_project['project'],
            temp_project['build']
        )
        result = scanner.scan()

        cpp_file = result.sketch_sources[0]
        content = cpp_file.read_text()

        # Comments should be preserved
        assert '// This is a comment' in content
        assert '/* Multi-line' in content

    def test_preprocess_with_preprocessor_directives(self, temp_project):
        """Test preprocessing preserves #define, #include."""
        ino_file = temp_project['src'] / "sketch.ino"
        ino_file.write_text("""
#define LED_PIN 13
#include "config.h"

void setup() {
  pinMode(LED_PIN, OUTPUT);
}

void loop() {
}
""")

        scanner = SourceScanner(
            temp_project['project'],
            temp_project['build']
        )
        result = scanner.scan()

        cpp_file = result.sketch_sources[0]
        content = cpp_file.read_text()

        # Preprocessor directives should be preserved
        assert '#define LED_PIN 13' in content
        assert '#include "config.h"' in content

    def test_source_collection_all_sources(self, temp_project):
        """Test SourceCollection.all_sources() method."""
        collection = SourceCollection(
            sketch_sources=[Path("sketch.cpp")],
            core_sources=[Path("wiring.c"), Path("main.cpp")],
            variant_sources=[Path("pins.cpp")],
            headers=[Path("config.h")]
        )

        all_sources = collection.all_sources()
        assert len(all_sources) == 4
        assert Path("sketch.cpp") in all_sources
        assert Path("wiring.c") in all_sources
        assert Path("main.cpp") in all_sources
        assert Path("pins.cpp") in all_sources

    def test_nonexistent_source_directory(self, temp_project):
        """Test scanning when source directory doesn't exist."""
        scanner = SourceScanner(
            temp_project['project'],
            temp_project['build']
        )
        result = scanner.scan(src_dir=temp_project['project'] / 'nonexistent')

        assert len(result.sketch_sources) == 0

    def test_nonexistent_core_directory(self, temp_project):
        """Test scanning when core directory doesn't exist."""
        scanner = SourceScanner(
            temp_project['project'],
            temp_project['build']
        )
        result = scanner.scan(core_dir=temp_project['project'] / 'nonexistent')

        assert len(result.core_sources) == 0

    def test_function_prototype_extraction_complex(self, temp_project):
        """Test extracting function prototypes with complex signatures."""
        ino_file = temp_project['src'] / "complex.ino"
        ino_file.write_text("""
void setup() {}

unsigned long getTime() {
  return millis();
}

float* getPointer(int* value, const char* name) {
  return nullptr;
}

template<typename T>
T getValue(T input) {
  return input;
}
""")

        scanner = SourceScanner(
            temp_project['project'],
            temp_project['build']
        )
        result = scanner.scan()

        cpp_file = result.sketch_sources[0]
        content = cpp_file.read_text()

        # Check prototypes for complex types
        assert 'void setup();' in content
        assert 'unsigned long getTime();' in content
        assert 'float* getPointer(int* value, const char* name);' in content
