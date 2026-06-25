def public_api():
    return _helper()


def _helper():
    return "ok"


def _dead_internal():
    return "remove me"


class PublicClass:
    def __init__(self):
        self.value = _helper()


class _DeadInternal:
    pass
