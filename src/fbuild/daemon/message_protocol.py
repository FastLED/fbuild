"""
Message serialization protocol and utilities for fbuild daemon operations.

This module provides a protocol and helper functions for serializing and
deserializing dataclass-based messages with enum support. It enables type-safe
message passing between daemon and client components.

Features:
- SerializableMessage protocol for consistent serialization interface
- Automatic enum serialization/deserialization
- Support for nested SerializableMessage objects
- Proper handling of Optional[Enum] types
- Respects field defaults for optional fields
- Clear error messages for missing required fields
"""

import dataclasses
from dataclasses import fields, is_dataclass
from enum import Enum
from typing import Any, Protocol, Type, TypeVar, runtime_checkable

T = TypeVar("T")


@runtime_checkable
class SerializableMessage(Protocol):
    """Protocol for messages that can be serialized to/from dictionaries.

    Any message class implementing this protocol can be automatically
    serialized and deserialized using the helper functions in this module.

    All message dataclasses should implement these two methods:
    - to_dict(): Convert the message to a dictionary for JSON serialization
    - from_dict(cls, data): Create a message instance from a dictionary

    Example:
        @dataclass
        class MyMessage:
            value: str
            status: MyEnum

            def to_dict(self) -> dict[str, Any]:
                return serialize_dataclass(self)

            @classmethod
            def from_dict(cls, data: dict[str, Any]) -> "MyMessage":
                return deserialize_dataclass(cls, data)
    """

    def to_dict(self) -> dict[str, Any]:
        """Convert this message to a dictionary for JSON serialization.

        Returns:
            Dictionary representation of this message with enum values
            converted to strings and nested messages converted to dicts.
        """
        ...

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "SerializableMessage":
        """Create a message instance from a dictionary.

        Args:
            data: Dictionary containing message data (typically from JSON)

        Returns:
            New message instance populated with data from dictionary

        Raises:
            KeyError: If required fields are missing from data
            ValueError: If enum values are invalid
        """
        ...


def serialize_dataclass(obj: Any) -> dict[str, Any]:
    """Serialize a dataclass instance to a dictionary.

    This function handles:
    - Enum values (converts to .value)
    - Optional[Enum] types (converts to .value or None)
    - Nested SerializableMessage objects (recursively serializes)
    - Lists of values (recursively processes)
    - Basic types (str, int, float, bool, None)

    Args:
        obj: Dataclass instance to serialize

    Returns:
        Dictionary representation with all enum values converted to strings
        and nested messages converted to dictionaries

    Raises:
        TypeError: If obj is not a dataclass instance

    Example:
        @dataclass
        class Status:
            state: MyEnum
            message: str

        status = Status(state=MyEnum.ACTIVE, message="Running")
        data = serialize_dataclass(status)
        # Returns: {"state": "active", "message": "Running"}
    """
    if not is_dataclass(obj):
        raise TypeError(f"serialize_dataclass requires a dataclass instance, got {type(obj)}")

    result: dict[str, Any] = {}

    for field in fields(obj):
        value = getattr(obj, field.name)

        # Handle None values
        if value is None:
            result[field.name] = None
            continue

        # Handle Enum values - convert to string
        if isinstance(value, Enum):
            result[field.name] = value.value
            continue

        # Handle nested SerializableMessage objects
        if isinstance(value, SerializableMessage):
            result[field.name] = value.to_dict()
            continue

        # Handle lists - recursively process each element
        if isinstance(value, list):
            result[field.name] = [_serialize_value(item) for item in value]
            continue

        # Handle dictionaries - recursively process each value
        if isinstance(value, dict):
            result[field.name] = {k: _serialize_value(v) for k, v in value.items()}
            continue

        # Basic types pass through
        result[field.name] = value

    return result


def _serialize_value(value: Any) -> Any:
    """Helper to serialize a single value recursively.

    Args:
        value: Value to serialize

    Returns:
        Serialized value (enum -> string, message -> dict, or original value)
    """
    if value is None:
        return None
    if isinstance(value, Enum):
        return value.value
    if isinstance(value, SerializableMessage):
        return value.to_dict()
    if isinstance(value, list):
        return [_serialize_value(item) for item in value]
    if isinstance(value, dict):
        return {k: _serialize_value(v) for k, v in value.items()}
    return value


def deserialize_dataclass(cls: Type[T], data: dict[str, Any]) -> T:
    """Deserialize a dictionary into a dataclass instance.

    This function handles:
    - Enum conversion from string values
    - Optional[Enum] types (converts from string or None)
    - Nested SerializableMessage objects (recursively deserializes)
    - Field defaults (doesn't require optional fields in data)
    - Type validation

    Args:
        cls: Dataclass class to instantiate
        data: Dictionary containing field values

    Returns:
        New instance of cls populated with data from dictionary

    Raises:
        TypeError: If cls is not a dataclass
        KeyError: If required field (no default) is missing from data
        ValueError: If enum value is invalid or type conversion fails

    Example:
        @dataclass
        class Status:
            state: MyEnum
            message: str
            count: int = 0

        data = {"state": "active", "message": "Running"}
        status = deserialize_dataclass(Status, data)
        # Returns: Status(state=MyEnum.ACTIVE, message="Running", count=0)
    """
    if not is_dataclass(cls):
        raise TypeError(f"deserialize_dataclass requires a dataclass type, got {cls}")

    kwargs: dict[str, Any] = {}

    for field in fields(cls):
        field_name = field.name

        # Check if field is present in data
        if field_name not in data:
            # If field has a default or default_factory, it's optional
            if field.default is not dataclasses.MISSING or field.default_factory is not dataclasses.MISSING:
                continue
            # Required field is missing
            raise KeyError(f"Required field '{field_name}' missing from data for {cls.__name__}")

        value = data[field_name]

        # Handle None values
        if value is None:
            kwargs[field_name] = None
            continue

        # Try to get the field type
        field_type = field.type

        # Handle Optional types (Union[X, None] or X | None)
        # Extract the actual type if it's Optional
        origin = getattr(field_type, "__origin__", None)
        args = getattr(field_type, "__args__", ())

        # Check if this is a union type (either Union[X, None] or X | None)
        if origin is not None or (args and type(None) in args):
            # For Union types, get the args
            if type(None) in args:
                # This is Optional[T], extract T
                non_none_types = [arg for arg in args if arg is not type(None)]
                if non_none_types:
                    field_type = non_none_types[0]

        # Handle Enum fields - convert from string
        if isinstance(field_type, type) and issubclass(field_type, Enum):
            try:
                kwargs[field_name] = field_type(value)
            except ValueError as e:
                raise ValueError(f"Invalid enum value '{value}' for field '{field_name}' in {cls.__name__}: {e}")
            continue

        # Handle nested SerializableMessage objects
        if isinstance(field_type, type) and is_dataclass(field_type):
            # Check if the type implements SerializableMessage protocol
            if hasattr(field_type, "from_dict") and callable(getattr(field_type, "from_dict", None)):
                kwargs[field_name] = field_type.from_dict(value)  # type: ignore[attr-defined]
                continue

        # Handle lists - check if elements need deserialization
        if isinstance(value, list):
            kwargs[field_name] = [_deserialize_value(item, field_type) for item in value]
            continue

        # Handle dictionaries - pass through as-is (unless we need special handling)
        if isinstance(value, dict):
            # Check if this should be deserialized as a nested message
            if isinstance(field_type, type) and is_dataclass(field_type):
                if hasattr(field_type, "from_dict") and callable(getattr(field_type, "from_dict", None)):
                    kwargs[field_name] = field_type.from_dict(value)  # type: ignore[attr-defined]
                    continue
            kwargs[field_name] = value
            continue

        # Basic types pass through
        kwargs[field_name] = value

    return cls(**kwargs)


def _deserialize_value(value: Any, expected_type: Any) -> Any:
    """Helper to deserialize a single value with type information.

    Args:
        value: Value to deserialize
        expected_type: Expected type of the value

    Returns:
        Deserialized value (string -> enum, dict -> message, or original)
    """
    if value is None:
        return None

    # Handle Optional types (Union[X, None] or X | None)
    origin = getattr(expected_type, "__origin__", None)
    args = getattr(expected_type, "__args__", ())

    # Check if this is a union type (either Union[X, None] or X | None)
    if origin is not None or (args and type(None) in args):
        if type(None) in args:
            non_none_types = [arg for arg in args if arg is not type(None)]
            if non_none_types:
                expected_type = non_none_types[0]

    # Handle Enum conversion
    if isinstance(expected_type, type) and issubclass(expected_type, Enum):
        return expected_type(value)

    # Handle nested dataclass conversion
    if isinstance(expected_type, type) and is_dataclass(expected_type):
        if isinstance(value, dict) and hasattr(expected_type, "from_dict") and callable(getattr(expected_type, "from_dict", None)):
            return expected_type.from_dict(value)  # type: ignore[attr-defined]

    # Handle lists recursively
    if isinstance(value, list):
        # Try to extract list element type if available
        origin = getattr(expected_type, "__origin__", None)
        if origin is list:
            args = getattr(expected_type, "__args__", ())
            if args:
                element_type = args[0]
                return [_deserialize_value(item, element_type) for item in value]

    return value


class EnumSerializationMixin:
    """Mixin class providing automatic enum serialization/deserialization.

    This mixin can be added to dataclasses that contain enum fields to
    provide automatic to_dict/from_dict implementations using the helper
    functions in this module.

    The mixin handles all enum conversions automatically, including:
    - Direct enum fields (MyEnum)
    - Optional enum fields (MyEnum | None)
    - Lists of enums (list[MyEnum])
    - Nested SerializableMessage objects

    Example:
        @dataclass
        class MyMessage(EnumSerializationMixin):
            status: StatusEnum
            priority: PriorityEnum | None = None

        msg = MyMessage(status=StatusEnum.ACTIVE)
        data = msg.to_dict()  # {"status": "active", "priority": null}
        msg2 = MyMessage.from_dict(data)  # Reconstructs the object
    """

    def to_dict(self) -> dict[str, Any]:
        """Convert this message to a dictionary for JSON serialization.

        Uses serialize_dataclass to automatically handle all enum conversions
        and nested message serialization.

        Returns:
            Dictionary representation of this message
        """
        return serialize_dataclass(self)

    @classmethod
    def from_dict(cls: Type[T], data: dict[str, Any]) -> T:
        """Create a message instance from a dictionary.

        Uses deserialize_dataclass to automatically handle all enum conversions
        and nested message deserialization.

        Args:
            data: Dictionary containing message data

        Returns:
            New message instance populated with data

        Raises:
            KeyError: If required fields are missing
            ValueError: If enum values are invalid
        """
        return deserialize_dataclass(cls, data)
