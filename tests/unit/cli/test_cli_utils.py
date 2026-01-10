"""Unit tests for CLI utilities including BannerFormatter."""

from zapio.cli_utils import BannerFormatter


class TestBannerFormatter:
    """Tests for BannerFormatter class."""

    def test_format_banner_single_line_centered(self):
        """Test formatting a single-line banner with centered text."""
        result = BannerFormatter.format_banner("Hello World", width=20, center=True)
        lines = result.split("\n")

        assert len(lines) == 3
        assert lines[0] == "=" * 20
        assert lines[2] == "=" * 20
        assert "Hello World" in lines[1]
        # Check if text is approximately centered
        assert lines[1].strip() == "Hello World"

    def test_format_banner_single_line_left_aligned(self):
        """Test formatting a single-line banner with left-aligned text."""
        result = BannerFormatter.format_banner("Hello World", width=20, center=False)
        lines = result.split("\n")

        assert len(lines) == 3
        assert lines[0] == "=" * 20
        assert lines[2] == "=" * 20
        assert lines[1] == "  Hello World"

    def test_format_banner_multi_line(self):
        """Test formatting a multi-line banner."""
        message = "Line 1\nLine 2\nLine 3"
        result = BannerFormatter.format_banner(message, width=30, center=False)
        lines = result.split("\n")

        assert len(lines) == 5  # 2 borders + 3 content lines
        assert lines[0] == "=" * 30
        assert lines[4] == "=" * 30
        assert lines[1] == "  Line 1"
        assert lines[2] == "  Line 2"
        assert lines[3] == "  Line 3"

    def test_format_banner_custom_border_char(self):
        """Test formatting a banner with custom border character."""
        result = BannerFormatter.format_banner(
            "Test", width=10, border_char="-", center=False
        )
        lines = result.split("\n")

        assert lines[0] == "-" * 10
        assert lines[2] == "-" * 10

    def test_format_banner_default_width(self):
        """Test formatting with default width (80 columns)."""
        result = BannerFormatter.format_banner("Test", center=False)
        lines = result.split("\n")

        assert lines[0] == "=" * 80
        assert lines[2] == "=" * 80

    def test_format_banner_default_border_char(self):
        """Test formatting with default border character (=)."""
        result = BannerFormatter.format_banner("Test", width=20, center=False)
        lines = result.split("\n")

        assert lines[0] == "=" * 20
        assert lines[2] == "=" * 20

    def test_format_banner_centered_multi_line(self):
        """Test formatting a multi-line banner with centered text."""
        message = "Success!\nBuild complete"
        result = BannerFormatter.format_banner(message, width=40, center=True)
        lines = result.split("\n")

        assert len(lines) == 4  # 2 borders + 2 content lines
        assert lines[0] == "=" * 40
        assert lines[3] == "=" * 40
        # Both lines should be centered
        assert "Success!" in lines[1]
        assert "Build complete" in lines[2]

    def test_format_banner_width_60(self):
        """Test formatting with 60-character width (common in orchestrator files)."""
        result = BannerFormatter.format_banner(
            "BUILD SUCCESSFUL!", width=60, center=False
        )
        lines = result.split("\n")

        assert len(lines) == 3
        assert lines[0] == "=" * 60
        assert lines[2] == "=" * 60
        assert lines[1] == "  BUILD SUCCESSFUL!"

    def test_format_banner_complex_message(self):
        """Test formatting a complex multi-line message like build output."""
        message = "BUILD SUCCESSFUL!\nBuild time: 3.45s\nFirmware: firmware.hex"
        result = BannerFormatter.format_banner(message, width=60, center=False)
        lines = result.split("\n")

        assert len(lines) == 5  # 2 borders + 3 content lines
        assert lines[0] == "=" * 60
        assert lines[4] == "=" * 60
        assert lines[1] == "  BUILD SUCCESSFUL!"
        assert lines[2] == "  Build time: 3.45s"
        assert lines[3] == "  Firmware: firmware.hex"

    def test_format_banner_empty_string(self):
        """Test formatting an empty string."""
        result = BannerFormatter.format_banner("", width=20, center=False)
        lines = result.split("\n")

        assert len(lines) == 3
        assert lines[0] == "=" * 20
        assert lines[2] == "=" * 20
        assert lines[1] == "  "

    def test_format_banner_very_long_line(self):
        """Test formatting a line longer than the banner width."""
        long_text = "x" * 100
        result = BannerFormatter.format_banner(long_text, width=20, center=False)
        lines = result.split("\n")

        assert len(lines) == 3
        assert lines[0] == "=" * 20
        assert lines[2] == "=" * 20
        # The long line should still be included, even if it overflows
        assert lines[1] == "  " + long_text

    def test_print_banner(self, capsys):
        """Test that print_banner outputs correctly."""
        BannerFormatter.print_banner("Test Message", width=30, center=False)
        captured = capsys.readouterr()

        output_lines = captured.out.strip().split("\n")
        # Should have blank line, border, message, border
        assert len(output_lines) == 3
        assert output_lines[0] == "=" * 30
        assert output_lines[1] == "  Test Message"
        assert output_lines[2] == "=" * 30

    def test_print_banner_multi_line(self, capsys):
        """Test that print_banner handles multi-line messages."""
        message = "Line 1\nLine 2"
        BannerFormatter.print_banner(message, width=25, center=False)
        captured = capsys.readouterr()

        output_lines = captured.out.strip().split("\n")
        assert len(output_lines) == 4  # 2 borders + 2 content lines
        assert output_lines[0] == "=" * 25
        assert output_lines[1] == "  Line 1"
        assert output_lines[2] == "  Line 2"
        assert output_lines[3] == "=" * 25
