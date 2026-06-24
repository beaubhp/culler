module_value = 0


def outer(param):
    outer_value = param

    def uses_free():
        return outer_value + module_value + missing_global

    def writes_global():
        global module_value
        module_value = module_value + 1
        return module_value

    def writes_nonlocal():
        nonlocal outer_value
        outer_value = outer_value + 1
        return outer_value

    class Nested:
        from_function = outer_value
        local_shadow = local_shadow

    return uses_free, writes_global, writes_nonlocal, Nested
