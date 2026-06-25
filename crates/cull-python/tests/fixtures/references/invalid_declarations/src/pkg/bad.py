def use_before_global():
    value
    global value
    return value


def assign_before_nonlocal():
    value = 1
    nonlocal value
    return value


def missing_nonlocal():
    nonlocal absent
    return absent


def parameter_conflict(value):
    global value
    return value


def conflicting():
    global both
    nonlocal both
    return both
