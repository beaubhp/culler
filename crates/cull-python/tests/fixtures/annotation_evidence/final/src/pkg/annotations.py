from typing import TYPE_CHECKING, Literal, overload
import typing as t

if TYPE_CHECKING:
    from pkg.types import TypeOnlyImport
    type_only_value = TypeOnlyImport
else:
    runtime_value = 1

if not t.TYPE_CHECKING:
    runtime_negative = 2
else:
    from pkg.types import NegativeTypeOnly


class User:
    pass


class Node:
    pass


def decorator(fn):
    return fn


def default_factory():
    return User


@decorator
def eager_annotation(value: User = default_factory()) -> Node:
    return Node()


def string_annotation(value: "User | None") -> list["Node"]:
    return []


def literal_annotation(value: Literal["active"]) -> None:
    return None


class GenericBox[T: User]:
    alias: T


type Alias[T = User] = list[T]


@overload
def overloaded(value: int) -> int:
    ...


@overload
def overloaded(value: str) -> str:
    ...


def overloaded(value):
    return value


def referenced_by_type_checking(value: TypeOnlyImport) -> NegativeTypeOnly:
    return value


def unreferenced():
    return None
