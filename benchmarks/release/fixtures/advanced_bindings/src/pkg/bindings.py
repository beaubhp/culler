FLAG = True


def replaced():
    return "old"


def replaced():
    return "new"


def chosen():
    return replaced()


if FLAG:

    def branch_target():
        return "left"

else:

    def branch_target():
        return "right"


def branch_user():
    return branch_target()


def marker(fn):
    return fn


@marker
def decorated():
    return "decorated"


class Base:
    pass


class Child(Base):
    pass


def child_value():
    return Child()


def unused_after_branch():
    return "dead"
