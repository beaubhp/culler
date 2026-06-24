global_slot = 0
module_fallback = 1
eager_source = 2
items = [1, 2]

if cond:
    module_partial = 1
use_module_partial(module_partial)


def sequential():
    seq_value = 1
    use_seq_first(seq_value)
    seq_value = 2
    use_seq_second(seq_value)


def branches(flag):
    if flag:
        branch_value = 1
    else:
        branch_value = 2
    use_branch(branch_value)

    if flag:
        partial_value = 1
    use_partial(partial_value)


def deletion():
    deleted_value = 1
    del deleted_value
    use_deleted(deleted_value)


def unreachable():
    return
    use_unreachable(global_slot)


def global_flow():
    global global_slot
    global_slot = 1
    use_global_known(global_slot)
    maybe_mutate()
    use_global_after_call(global_slot)


def conditional_global(flag):
    global global_slot
    if flag:
        global_slot = 2
    use_conditional_global(global_slot)


def outer_nonlocal(flag):
    outer_value = 0

    def inner():
        nonlocal outer_value
        if flag:
            outer_value = 1
        use_nonlocal(outer_value)

    return inner


def exception_flow():
    try:
        try_value = 1
        maybe_raise()
    except Error as exc_value:
        use_exc_inside(exc_value)
    use_exc_after(exc_value)
    use_try_value(try_value)


def match_flow(subject):
    match subject:
        case {"key": captured_value}:
            use_captured_inside(captured_value)
    use_captured_after(captured_value)


class ClassFlow:
    if cond:
        maybe_class = 1
    use_class_fallback(maybe_class)
    use_global_fallback(module_fallback)


eager_values = [eager_source for item in items]
generator_values = (eager_source for item in items)
