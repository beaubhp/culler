decorator = make_decorator()
default_value = object()
base_type = object()
meta_type = type
module_value = 1
xs = [1, 2, 3]
x = 10


@decorator
def with_default(value=default_value):
    local = value
    return module_value + local + missing_global


@decorator
class ContextCase(base_type, metaclass=meta_type):
    __private = x
    class_value = module_value
    leftmost = [item for item in xs]
    no_class_capture = [x for _ in range(3)]
    class_private = __private

    def method(self):
        return __private + class_value


lambda_value = (lambda item: item + module_value)(1)
