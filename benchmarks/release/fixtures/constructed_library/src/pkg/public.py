from pkg.private import _connect_impl


def connect():
    return _connect_impl()


class Client:
    def connect(self):
        return connect()


def experimental_public():
    pass
