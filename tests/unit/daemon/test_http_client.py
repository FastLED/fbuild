"""
Comprehensive Unit Tests for HTTP Daemon Client

Tests the HTTP-based daemon communication layer, including:
- Port discovery (environment variable, port file, dev mode)
- HTTP client configuration
- Request/response serialization
- Error handling (connection errors, timeouts, invalid responses)
- Daemon availability checks
"""

import os
from pathlib import Path
from unittest.mock import MagicMock, Mock, patch

import httpx
import pytest

from fbuild.daemon.client.http_utils import (
    DEFAULT_DEV_PORT,
    DEFAULT_PORT,
    deserialize_response,
    get_daemon_base_url,
    get_daemon_port,
    get_daemon_url,
    http_client,
    is_daemon_http_available,
    serialize_request,
    wait_for_daemon_http,
)
from fbuild.daemon.messages import BuildRequest


class TestPortDiscovery:
    """Test port discovery from environment variables, port file, and defaults."""

    def test_port_from_env_variable(self, tmp_path: Path):
        """Test port discovery from FBUILD_DAEMON_PORT environment variable."""
        with patch.dict(os.environ, {"FBUILD_DAEMON_PORT": "9176"}):
            port = get_daemon_port()
            assert port == 9176

    def test_port_from_env_variable_invalid(self):
        """Test that invalid FBUILD_DAEMON_PORT is ignored and fallback is used."""
        with patch.dict(os.environ, {"FBUILD_DAEMON_PORT": "invalid"}):
            port = get_daemon_port()
            # Should fall back to default
            assert port in [DEFAULT_PORT, DEFAULT_DEV_PORT]

    def test_port_from_env_variable_out_of_range(self):
        """Test that out-of-range FBUILD_DAEMON_PORT is ignored."""
        with patch.dict(os.environ, {"FBUILD_DAEMON_PORT": "99999"}):
            port = get_daemon_port()
            # Should fall back to default
            assert port in [DEFAULT_PORT, DEFAULT_DEV_PORT]

    def test_port_from_file(self, tmp_path: Path):
        """Test port discovery from port file."""
        port_file = tmp_path / "daemon.port"
        port_file.write_text("9176")

        with patch("fbuild.daemon.client.http_utils.PORT_FILE", port_file):
            # Clear environment variable to test port file priority
            with patch.dict(os.environ, {}, clear=True):
                port = get_daemon_port()
                assert port == 9176

    def test_port_from_file_invalid(self, tmp_path: Path):
        """Test that invalid port file is ignored."""
        port_file = tmp_path / "daemon.port"
        port_file.write_text("invalid")

        with patch("fbuild.daemon.client.http_utils.PORT_FILE", port_file):
            with patch.dict(os.environ, {}, clear=True):
                port = get_daemon_port()
                # Should fall back to default
                assert port in [DEFAULT_PORT, DEFAULT_DEV_PORT]

    def test_port_dev_mode(self, tmp_path: Path):
        """Test port discovery in dev mode."""
        # Create a non-existent port file to ensure we test fallback to dev mode default
        port_file = tmp_path / "daemon.port"

        with patch("fbuild.daemon.client.http_utils.PORT_FILE", port_file):
            with patch.dict(os.environ, {"FBUILD_DEV_MODE": "1"}, clear=True):
                port = get_daemon_port()
                assert port == DEFAULT_DEV_PORT

    def test_port_production_mode(self, tmp_path: Path):
        """Test port discovery in production mode."""
        # Create a non-existent port file path
        port_file = tmp_path / "daemon.port"

        # Remove FBUILD_DEV_MODE and FBUILD_DAEMON_PORT to test production mode
        env_vars = {k: v for k, v in os.environ.items() if k not in ["FBUILD_DEV_MODE", "FBUILD_DAEMON_PORT"]}
        with patch("fbuild.daemon.client.http_utils.PORT_FILE", port_file):
            with patch.dict(os.environ, env_vars, clear=True):
                port = get_daemon_port()
                assert port == DEFAULT_PORT

    def test_port_priority_env_over_file(self, tmp_path: Path):
        """Test that FBUILD_DAEMON_PORT takes priority over port file."""
        port_file = tmp_path / "daemon.port"
        port_file.write_text("8888")

        with patch("fbuild.daemon.client.http_utils.PORT_FILE", port_file):
            with patch.dict(os.environ, {"FBUILD_DAEMON_PORT": "9176"}):
                port = get_daemon_port()
                assert port == 9176  # Environment variable takes priority

    def test_port_priority_file_over_dev_mode(self, tmp_path: Path):
        """Test that port file takes priority over dev mode default."""
        port_file = tmp_path / "daemon.port"
        port_file.write_text("9176")

        with patch("fbuild.daemon.client.http_utils.PORT_FILE", port_file):
            with patch.dict(os.environ, {"FBUILD_DEV_MODE": "1"}, clear=True):
                port = get_daemon_port()
                assert port == 9176  # Port file takes priority


class TestURLGeneration:
    """Test URL generation for daemon endpoints."""

    def test_get_daemon_base_url_with_env_port(self):
        """Test base URL generation with environment variable port."""
        with patch.dict(os.environ, {"FBUILD_DAEMON_PORT": "9176"}):
            url = get_daemon_base_url()
            assert url == "http://127.0.0.1:9176"

    def test_get_daemon_base_url_dev_mode(self, tmp_path: Path):
        """Test base URL generation in dev mode."""
        # Create a non-existent port file to ensure we test fallback to dev mode default
        port_file = tmp_path / "daemon.port"

        with patch("fbuild.daemon.client.http_utils.PORT_FILE", port_file):
            with patch.dict(os.environ, {"FBUILD_DEV_MODE": "1"}, clear=True):
                url = get_daemon_base_url()
                assert url == f"http://127.0.0.1:{DEFAULT_DEV_PORT}"

    def test_get_daemon_base_url_production(self, tmp_path: Path):
        """Test base URL generation in production mode."""
        # Create a non-existent port file path
        port_file = tmp_path / "daemon.port"

        # Remove FBUILD_DEV_MODE and FBUILD_DAEMON_PORT to test production mode
        env_vars = {k: v for k, v in os.environ.items() if k not in ["FBUILD_DEV_MODE", "FBUILD_DAEMON_PORT"]}
        with patch("fbuild.daemon.client.http_utils.PORT_FILE", port_file):
            with patch.dict(os.environ, env_vars, clear=True):
                url = get_daemon_base_url()
                assert url == f"http://127.0.0.1:{DEFAULT_PORT}"

    def test_get_daemon_url_with_path(self):
        """Test URL generation with endpoint path."""
        with patch.dict(os.environ, {"FBUILD_DAEMON_PORT": "9176"}):
            url = get_daemon_url("/api/build")
            assert url == "http://127.0.0.1:9176/api/build"

    def test_get_daemon_url_without_leading_slash(self):
        """Test URL generation when path doesn't start with /."""
        with patch.dict(os.environ, {"FBUILD_DAEMON_PORT": "9176"}):
            url = get_daemon_url("api/build")
            assert url == "http://127.0.0.1:9176/api/build"

    def test_get_daemon_url_empty_path(self):
        """Test URL generation with empty path."""
        with patch.dict(os.environ, {"FBUILD_DAEMON_PORT": "9176"}):
            url = get_daemon_url("")
            assert url == "http://127.0.0.1:9176"


class TestHTTPClientConfiguration:
    """Test HTTP client configuration and creation."""

    def test_http_client_default_timeout(self):
        """Test HTTP client with default timeout."""
        client = http_client()
        assert isinstance(client, httpx.Client)
        assert client.timeout.read == 30.0

    def test_http_client_custom_timeout(self):
        """Test HTTP client with custom timeout."""
        client = http_client(timeout=60.0, connect_timeout=10.0)
        assert isinstance(client, httpx.Client)
        assert client.timeout.read == 60.0
        assert client.timeout.connect == 10.0

    def test_http_client_follows_redirects(self):
        """Test that HTTP client follows redirects."""
        client = http_client()
        assert client.follow_redirects is True


class TestDaemonAvailability:
    """Test daemon availability checks."""

    def test_is_daemon_http_available_success(self):
        """Test daemon availability check when daemon is running."""
        mock_response = Mock()
        mock_response.status_code = 200

        with patch("fbuild.daemon.client.http_utils.http_client") as mock_client_factory:
            mock_client = MagicMock()
            mock_client.__enter__ = Mock(return_value=mock_client)
            mock_client.__exit__ = Mock(return_value=False)
            mock_client.get.return_value = mock_response
            mock_client_factory.return_value = mock_client

            assert is_daemon_http_available() is True

    def test_is_daemon_http_available_connection_error(self):
        """Test daemon availability check when connection fails."""
        with patch("fbuild.daemon.client.http_utils.http_client") as mock_client_factory:
            mock_client = MagicMock()
            mock_client.__enter__ = Mock(return_value=mock_client)
            mock_client.__exit__ = Mock(return_value=False)
            mock_client.get.side_effect = httpx.ConnectError("Connection refused")
            mock_client_factory.return_value = mock_client

            assert is_daemon_http_available() is False

    def test_is_daemon_http_available_timeout(self):
        """Test daemon availability check when request times out."""
        with patch("fbuild.daemon.client.http_utils.http_client") as mock_client_factory:
            mock_client = MagicMock()
            mock_client.__enter__ = Mock(return_value=mock_client)
            mock_client.__exit__ = Mock(return_value=False)
            mock_client.get.side_effect = httpx.TimeoutException("Timeout")
            mock_client_factory.return_value = mock_client

            assert is_daemon_http_available() is False

    def test_is_daemon_http_available_http_error(self):
        """Test daemon availability check when HTTP error occurs."""
        mock_response = Mock()
        mock_response.status_code = 500

        with patch("fbuild.daemon.client.http_utils.http_client") as mock_client_factory:
            mock_client = MagicMock()
            mock_client.__enter__ = Mock(return_value=mock_client)
            mock_client.__exit__ = Mock(return_value=False)
            mock_client.get.return_value = mock_response
            mock_client_factory.return_value = mock_client

            assert is_daemon_http_available() is False


class TestWaitForDaemon:
    """Test waiting for daemon to become available."""

    def test_wait_for_daemon_http_immediate_success(self):
        """Test waiting when daemon is immediately available."""
        with patch("fbuild.daemon.client.http_utils.is_daemon_http_available", return_value=True):
            result = wait_for_daemon_http(timeout=5.0, poll_interval=0.1)
            assert result is True

    def test_wait_for_daemon_http_timeout(self):
        """Test waiting when daemon never becomes available."""
        with patch("fbuild.daemon.client.http_utils.is_daemon_http_available", return_value=False):
            result = wait_for_daemon_http(timeout=0.5, poll_interval=0.1)
            assert result is False

    def test_wait_for_daemon_http_eventual_success(self):
        """Test waiting when daemon becomes available after some time."""
        call_count = 0

        def mock_available():
            nonlocal call_count
            call_count += 1
            return call_count >= 3  # Become available on 3rd call

        with patch("fbuild.daemon.client.http_utils.is_daemon_http_available", side_effect=mock_available):
            result = wait_for_daemon_http(timeout=5.0, poll_interval=0.1)
            assert result is True


class TestRequestResponseSerialization:
    """Test request/response serialization helpers."""

    def test_serialize_request(self):
        """Test serializing a request object."""
        import os

        request = BuildRequest(
            project_dir="/path/to/project",
            environment="uno",
            clean_build=False,
            verbose=False,
            caller_pid=os.getpid(),
            caller_cwd=os.getcwd(),
            jobs=4,
        )

        data = serialize_request(request)
        assert isinstance(data, dict)
        assert data["project_dir"] == "/path/to/project"
        assert data["environment"] == "uno"
        assert data["clean_build"] is False
        assert data["verbose"] is False
        assert data["jobs"] == 4

    def test_serialize_request_without_to_dict(self):
        """Test that serialize_request raises TypeError for invalid objects."""

        class InvalidRequest:
            pass

        with pytest.raises(TypeError, match="must implement to_dict"):
            serialize_request(InvalidRequest())

    def test_deserialize_response(self):
        """Test deserializing a response object (using BuildRequest as example)."""
        import os

        data = {
            "project_dir": "/path/to/project",
            "environment": "uno",
            "clean_build": False,
            "verbose": True,
            "caller_pid": os.getpid(),
            "caller_cwd": os.getcwd(),
            "jobs": 4,
        }

        request = deserialize_response(data, BuildRequest)
        assert isinstance(request, BuildRequest)
        assert request.project_dir == "/path/to/project"
        assert request.environment == "uno"
        assert request.clean_build is False
        assert request.verbose is True
        assert request.jobs == 4

    def test_deserialize_response_without_from_dict(self):
        """Test that deserialize_response raises TypeError for invalid classes."""

        class InvalidResponse:
            pass

        with pytest.raises(TypeError, match="must implement from_dict"):
            deserialize_response({}, InvalidResponse)


class TestHTTPClientMocking:
    """Test HTTP client with proper mocking (no actual network calls)."""

    def test_mock_successful_request(self):
        """Test successful HTTP request with mocked httpx."""
        mock_response = Mock()
        mock_response.status_code = 200
        mock_response.json.return_value = {"status": "healthy"}

        with patch("httpx.Client") as mock_client_class:
            mock_client_instance = Mock()
            mock_client_instance.get.return_value = mock_response
            mock_client_class.return_value.__enter__.return_value = mock_client_instance

            with patch.dict(os.environ, {"FBUILD_DAEMON_PORT": "9176"}):
                with http_client() as client:
                    response = client.get(get_daemon_url("/health"))
                    assert response.status_code == 200
                    assert response.json() == {"status": "healthy"}

    def test_mock_connection_refused(self):
        """Test handling of connection refused error with mocked httpx."""
        with patch("httpx.Client") as mock_client_class:
            mock_client_instance = Mock()
            mock_client_instance.get.side_effect = httpx.ConnectError("Connection refused")
            mock_client_class.return_value.__enter__.return_value = mock_client_instance

            with pytest.raises(httpx.ConnectError):
                with http_client() as client:
                    client.get(get_daemon_url("/health"))

    def test_mock_timeout_error(self):
        """Test handling of timeout error with mocked httpx."""
        with patch("httpx.Client") as mock_client_class:
            mock_client_instance = Mock()
            mock_client_instance.get.side_effect = httpx.TimeoutException("Timeout")
            mock_client_class.return_value.__enter__.return_value = mock_client_instance

            with pytest.raises(httpx.TimeoutException):
                with http_client() as client:
                    client.get(get_daemon_url("/health"))


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
