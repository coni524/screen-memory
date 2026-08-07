import pytest
from botocore.exceptions import ClientError

import profile_loader


class FakeBody:
    def __init__(self, data: bytes):
        self._data = data

    def read(self):
        return self._data


class FakeS3:
    """Stub that counts head_object / get_object calls."""

    def __init__(self, body: bytes | None, etag: str = '"etag-1"'):
        self.body = body  # None means the object does not exist
        self.etag = etag
        self.head_calls = 0
        self.get_calls = 0

    def _not_found(self, operation):
        return ClientError({"Error": {"Code": "404", "Message": "Not Found"}}, operation)

    def head_object(self, Bucket, Key):
        self.head_calls += 1
        if self.body is None:
            raise self._not_found("HeadObject")
        return {"ETag": self.etag}

    def get_object(self, Bucket, Key):
        self.get_calls += 1
        if self.body is None:
            raise self._not_found("GetObject")
        return {"ETag": self.etag, "Body": FakeBody(self.body)}


PROFILE_YAML = b"""
categories:
  - id: project-a
    label: "\xe6\xa1\x88\xe4\xbb\xb6A"
    criteria: repo-a
"""


@pytest.fixture(autouse=True)
def reset_cache():
    profile_loader._cache = None


def load(s3, now):
    return profile_loader.load_profile(s3, "bucket", "config/profile.yaml", now=now)


def test_オブジェクト不在なら既定分類を返す():
    s3 = FakeS3(None)
    categories, report_rules = load(s3, now=0)
    assert [c["id"] for c in categories] == ["coding", "writing", "browsing", "meeting", "chat", "other"]
    assert report_rules is None


def test_プロファイルの_categories_が既定を置き換え_other_を補う():
    s3 = FakeS3(PROFILE_YAML)
    categories, _ = load(s3, now=0)
    assert [c["id"] for c in categories] == ["project-a", "other"]
    assert categories[0]["criteria"] == "repo-a"


def test_5分以内はキャッシュを返し_S3_を呼ばない():
    s3 = FakeS3(PROFILE_YAML)
    load(s3, now=0)
    load(s3, now=299)
    assert s3.head_calls == 0
    assert s3.get_calls == 1


def test_5分経過後_ETag_が同じなら_GetObject_しない():
    s3 = FakeS3(PROFILE_YAML)
    load(s3, now=0)
    load(s3, now=301)
    assert s3.head_calls == 1
    assert s3.get_calls == 1


def test_5分経過後_ETag_が変わっていたら再取得する():
    s3 = FakeS3(PROFILE_YAML)
    load(s3, now=0)
    s3.body = b"categories:\n  - id: project-b\n    label: B\n"
    s3.etag = '"etag-2"'
    categories, _ = load(s3, now=301)
    assert s3.get_calls == 2
    assert categories[0]["id"] == "project-b"


def test_ETag_確認後は次の5分間キャッシュが有効():
    s3 = FakeS3(PROFILE_YAML)
    load(s3, now=0)
    load(s3, now=301)
    load(s3, now=302)
    assert s3.head_calls == 1


def test_YAML_破損なら既定分類にフォールバックする(capsys):
    s3 = FakeS3(b"categories: [broken")
    categories, report_rules = load(s3, now=0)
    assert [c["id"] for c in categories] == ["coding", "writing", "browsing", "meeting", "chat", "other"]
    assert report_rules is None
    assert "profile_load_error" in capsys.readouterr().out


def test_report_節を返す():
    s3 = FakeS3(b"report:\n  rules:\n    - rule1\n")
    _, report_rules = load(s3, now=0)
    assert report_rules == {"rules": ["rule1"]}
