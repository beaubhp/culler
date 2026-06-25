from pkg.commands import run_convert, run_check


def main():
    run_convert()
    run_check()


def legacy_entry():
    old_helper()


def old_helper():
    pass


def unused_command():
    pass
