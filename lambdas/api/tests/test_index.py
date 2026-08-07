import io
import json

import pytest

import index


class NoSuchKey(Exception):
    pass


class FakeS3:
    class exceptions:
        NoSuchKey = NoSuchKey

    def __init__(self, objects=None):
        self.objects = objects or {}

    def get_object(self, Bucket, Key):
        if Key not in self.objects:
            raise NoSuchKey()
        return {"Body": io.BytesIO(self.objects[Key].encode())}


class FakeDynamoDB:
    def __init__(self, pages):
        self.pages = pages
        self.queries = []

    def query(self, **kwargs):
        self.queries.append(kwargs)
        return self.pages[len(self.queries) - 1]


def db_item(hhmmss, device="mac-main", app="Code", summary="作業"):
    return {
        "pk": {"S": "DAY#2026-08-01"},
        "sk": {"S": f"TS#{hhmmss}#{device}"},
        "capturedAt": {"S": f"2026-08-01T{hhmmss}+09:00"},
        "device": {"S": device},
        "app": {"S": app},
        "windowTitle": {"S": "win"},
        "category": {"S": "coding"},
        "summary": {"S": summary},
        "ocrText": {"S": "text"},
        "imageKey": {"S": f"raw/{device}/2026/08/01/{hhmmss.replace(':', '')}.webp"},
    }


def make_event(route="GET /days/{date}/records", date="2026-08-01", query=None):
    return {
        "routeKey": route,
        "headers": {},
        "pathParameters": {"date": date},
        "queryStringParameters": query or {},
    }


@pytest.fixture
def fakes(monkeypatch):
    s3 = FakeS3({"reports/2026/08/01.md": "# 日報 2026-08-01\n本文"})
    dynamodb = FakeDynamoDB([{"Items": [db_item("10:00:00")]}])
    monkeypatch.setattr(index, "s3", s3)
    monkeypatch.setattr(index, "dynamodb", dynamodb)
    return None, s3, dynamodb


def body(response):
    return json.loads(response["body"])


class TestDateValidation:
    @pytest.mark.parametrize("date", ["2026/08/01", "2026-8-1", "20260801", ""])
    def test_形式不正は400(self, fakes, date):
        response = index.handler(make_event(date=date), None)
        assert response["statusCode"] == 400
        assert body(response) == {"message": "invalid date"}


class TestRecords:
    def test_全レコードを返す(self, fakes):
        response = index.handler(make_event(), None)
        payload = body(response)
        assert payload["date"] == "2026-08-01"
        assert payload["count"] == 1
        record = payload["records"][0]
        assert record["device"] == "mac-main"
        assert record["capturedAt"] == "2026-08-01T10:00:00+09:00"
        assert "pk" not in record and "sk" not in record

    def test_device_で絞り込む(self, fakes):
        _, _, dynamodb = fakes
        dynamodb.pages = [
            {"Items": [db_item("10:00:00"), db_item("10:01:00", device="win-sub")]}
        ]
        payload = body(
            index.handler(make_event(query={"device": "win-sub"}), None)
        )
        assert payload["count"] == 1
        assert payload["records"][0]["device"] == "win-sub"

    def test_ページネーションを結合する(self, fakes):
        _, _, dynamodb = fakes
        dynamodb.pages = [
            {
                "Items": [db_item("10:00:00")],
                "LastEvaluatedKey": {"pk": {"S": "DAY#2026-08-01"}},
            },
            {"Items": [db_item("10:01:00")]},
        ]
        payload = body(index.handler(make_event(), None))
        assert payload["count"] == 2
        assert len(dynamodb.queries) == 2
        assert "ExclusiveStartKey" in dynamodb.queries[1]

    def test_0件でも200で空リスト(self, fakes):
        _, _, dynamodb = fakes
        dynamodb.pages = [{"Items": []}]
        payload = body(index.handler(make_event(), None))
        assert payload == {"date": "2026-08-01", "count": 0, "records": []}

    def test_Decimal_は_int_に変換する(self, fakes):
        _, _, dynamodb = fakes
        item = db_item("10:00:00")
        item["ocrText"] = {"N": "123"}
        dynamodb.pages = [{"Items": [item]}]
        payload = body(index.handler(make_event(), None))
        assert payload["records"][0]["ocrText"] == 123


class TestReport:
    def test_JSONで返す(self, fakes):
        response = index.handler(
            make_event(route="GET /days/{date}/report"), None
        )
        assert response["statusCode"] == 200
        assert body(response) == {
            "date": "2026-08-01",
            "markdown": "# 日報 2026-08-01\n本文",
        }

    def test_format_md_はtext_markdownで本文をそのまま返す(self, fakes):
        response = index.handler(
            make_event(route="GET /days/{date}/report", query={"format": "md"}), None
        )
        assert response["statusCode"] == 200
        assert response["headers"]["content-type"] == "text/markdown; charset=utf-8"
        assert response["body"] == "# 日報 2026-08-01\n本文"

    def test_オブジェクトが無ければ404(self, fakes):
        response = index.handler(
            make_event(route="GET /days/{date}/report", date="2026-08-02"), None
        )
        assert response["statusCode"] == 404
        assert body(response) == {"message": "report not found"}


class TestErrors:
    def test_未知のルートは404(self, fakes):
        response = index.handler(make_event(route="GET /unknown"), None)
        assert response["statusCode"] == 404

    def test_内部エラーは500で詳細を含めない(self, fakes, capsys):
        _, _, dynamodb = fakes
        dynamodb.pages = []  # provokes an IndexError
        response = index.handler(make_event(), None)
        assert response["statusCode"] == 500
        assert body(response) == {"message": "internal error"}
        assert "IndexError" in capsys.readouterr().out
