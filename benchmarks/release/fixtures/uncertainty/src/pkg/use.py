import pkg.api as api

name = "handler"
getattr(api, name)
setattr(api, "maybe_dynamic", object())
exec("handler()")
