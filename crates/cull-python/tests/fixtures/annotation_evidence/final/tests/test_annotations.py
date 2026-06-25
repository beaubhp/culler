from pkg.annotations import string_annotation


def test_string_annotation():
    assert string_annotation(None) == []
