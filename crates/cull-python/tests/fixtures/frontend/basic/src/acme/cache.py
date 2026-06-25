from __future__ import annotations


class Cache[T]:
    pass


@decorator
async def refresh(key: str = "x") -> None:
    return None


def helper():
    def nested():
        return None

    return nested
