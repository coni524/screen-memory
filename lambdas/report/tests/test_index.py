import json
import re

import pytest

import index
import profile_loader


def record(seconds, device="mac-main", app="Code", category="coding", summary="作業"):
    return {
        "seconds": seconds,
        "device": device,
        "app": app,
        "category": category,
        "summary": summary,
    }


class TestResolveDate:
    def test_date_指定があればその日を使う(self):
        assert index.resolve_date({"date": "2026-08-01"}) == "2026-08-01"

    def test_無指定は今日のJST日付(self):
        assert re.fullmatch(r"\d{4}-\d{2}-\d{2}", index.resolve_date({}))
        assert re.fullmatch(r"\d{4}-\d{2}-\d{2}", index.resolve_date(None))

    @pytest.mark.parametrize(
        "raw", ["2026/08/01", "2026-8-1", "20260801", "", 20260801, "2026-13-01"]
    )
    def test_形式不正はエラー(self, raw):
        with pytest.raises(ValueError):
            index.resolve_date({"date": raw})


class TestBuildSegments:
    def test_継続時間は次のレコードまでで15分に打ち切る(self):
        # 10:00 -> 10:05 (5 min) -> 11:00 (55 min truncated to 15)
        segments = index.build_segments(
            [
                record(10 * 3600, category="coding"),
                record(10 * 3600 + 300, category="browsing"),
                record(11 * 3600, category="coding"),
            ]
        )
        assert [s["duration"] for s in segments] == [300, 900, 60]

    def test_最後のレコードは1分(self):
        segments = index.build_segments([record(10 * 3600)])
        assert segments[0]["duration"] == 60
        assert segments[0]["end_seconds"] == 10 * 3600 + 60

    def test_同じ_device_category_app_なら結合し_summary_は先頭(self):
        segments = index.build_segments(
            [
                record(10 * 3600, summary="先頭"),
                record(10 * 3600 + 60, summary="2件目"),
                record(10 * 3600 + 120, summary="3件目"),
            ]
        )
        assert len(segments) == 1
        assert segments[0]["summary"] == "先頭"
        assert segments[0]["records"] == 3
        assert segments[0]["duration"] == 180  # 60 + 60 + 60 for the last record

    @pytest.mark.parametrize(
        "kwargs", [{"device": "win-sub"}, {"category": "browsing"}, {"app": "Safari"}]
    )
    def test_device_category_app_のどれかが違えば結合しない(self, kwargs):
        segments = index.build_segments(
            [record(10 * 3600), record(10 * 3600 + 60, **kwargs)]
        )
        assert len(segments) == 2

    def test_ソートされていない入力も時刻順に集約する(self):
        segments = index.build_segments(
            [record(11 * 3600, category="browsing"), record(10 * 3600)]
        )
        assert segments[0]["category"] == "coding"


class TestThinSegments:
    def test_300以下はそのまま(self):
        segments = [{"minutes": 1} for _ in range(300)]
        kept, dropped = index.thin_segments(segments)
        assert len(kept) == 300
        assert dropped == 0

    def test_300超は_minutes_の小さい順に間引く(self):
        segments = [{"minutes": m, "id": i} for i, m in enumerate([5, 1, 3] * 101)]
        kept, dropped = index.thin_segments(segments)
        assert len(kept) == 300
        assert dropped == 3
        # The smallest segments (minutes=1) are dropped first
        assert sum(1 for s in kept if s["minutes"] == 1) == 101 - 3
        # The kept segments stay in their original order
        assert [s["id"] for s in kept] == sorted(s["id"] for s in kept)


class TestSegmentJson:
    def test_書式と分の丸め(self):
        seg = index.to_segment_json(
            {
                "start_seconds": 10 * 3600 + 23 * 60,
                "end_seconds": 11 * 3600 + 5 * 60,
                "duration": 42 * 60,
                "device": "mac-main",
                "app": "Code",
                "category": "coding",
                "summary": "作業",
                "records": 5,
            }
        )
        assert seg["start"] == "10:23"
        assert seg["end"] == "11:05"
        assert seg["minutes"] == 42

    def test_1分未満は1分に切り上げる(self):
        seg = index.to_segment_json(
            {
                "start_seconds": 0,
                "end_seconds": 20,
                "duration": 20,
                "device": "d",
                "app": "a",
                "category": "c",
                "summary": "s",
                "records": 1,
            }
        )
        assert seg["minutes"] == 1


class FakeDynamoDB:
    def __init__(self, pages):
        self.pages = pages
        self.queries = []
        self.items = []

    def query(self, **kwargs):
        self.queries.append(kwargs)
        page = self.pages[len(self.queries) - 1]
        return page

    def put_item(self, TableName, Item):
        self.items.append(Item)


class FakeS3:
    def __init__(self):
        self.objects = {}

    def put_object(self, Bucket, Key, Body, **kwargs):
        self.objects[Key] = Body


class FakeBedrock:
    def __init__(self, text="# 日報 2026-08-01\nやったこと"):
        self.text = text
        self.requests = []

    def converse(self, **kwargs):
        self.requests.append(kwargs)
        return {
            "output": {"message": {"content": [{"text": self.text}]}},
            "usage": {"inputTokens": 2000, "outputTokens": 500},
        }


def db_item(hhmmss, device="mac-main", app="Code", category="coding", summary="作業"):
    return {
        "pk": {"S": "DAY#2026-08-01"},
        "sk": {"S": f"TS#{hhmmss}#{device}"},
        "device": {"S": device},
        "app": {"S": app},
        "category": {"S": category},
        "summary": {"S": summary},
    }


@pytest.fixture
def fakes(monkeypatch):
    monkeypatch.setattr(
        profile_loader,
        "load_profile",
        lambda *a, **k: (list(profile_loader.DEFAULT_CATEGORIES), None),
    )
    dynamodb = FakeDynamoDB(
        [{"Items": [db_item("10:00:00"), db_item("10:05:00", category="browsing")]}]
    )
    s3 = FakeS3()
    bedrock = FakeBedrock()
    monkeypatch.setattr(index, "dynamodb", dynamodb)
    monkeypatch.setattr(index, "s3", s3)
    monkeypatch.setattr(index, "bedrock", bedrock)
    return dynamodb, s3, bedrock


def test_0件なら_report_skipped_で正常終了(fakes, capsys):
    dynamodb, s3, _ = fakes
    dynamodb.pages = [{"Items": []}]
    result = index.handler({"date": "2026-08-01"}, None)
    assert result == {"date": "2026-08-01", "skipped": True}
    assert "report_skipped" in capsys.readouterr().out
    assert s3.objects == {}
    assert dynamodb.items == []


def test_正常系で日報を保存しポインタを書く(fakes, capsys):
    dynamodb, s3, bedrock = fakes
    result = index.handler({"date": "2026-08-01"}, None)
    assert result == {"date": "2026-08-01", "reportKey": "reports/2026/08/01.md"}
    assert s3.objects["reports/2026/08/01.md"].decode().startswith("# 日報")
    pointer = dynamodb.items[0]
    assert pointer["pk"] == {"S": "DAY#2026-08-01"}
    assert pointer["sk"] == {"S": "REPORT"}
    assert pointer["reportKey"] == {"S": "reports/2026/08/01.md"}
    assert "generatedAt" in pointer
    out = capsys.readouterr().out
    assert "report_generated" in out
    request = bedrock.requests[0]
    assert request["inferenceConfig"] == {"maxTokens": 4096, "temperature": 0.3}
    assert "toolConfig" not in request


def test_ページネーションを全件たどる(fakes):
    dynamodb, _, _ = fakes
    dynamodb.pages = [
        {"Items": [db_item("10:00:00")], "LastEvaluatedKey": {"pk": {"S": "x"}}},
        {"Items": [db_item("10:05:00")]},
    ]
    index.handler({"date": "2026-08-01"}, None)
    assert len(dynamodb.queries) == 2
    assert "ExclusiveStartKey" in dynamodb.queries[1]


def test_生成結果が空なら例外(fakes):
    _, _, bedrock = fakes
    bedrock.text = "  \n"
    with pytest.raises(RuntimeError):
        index.handler({"date": "2026-08-01"}, None)


def test_プロファイルの_format_と_rules_をプロンプトに注入(fakes, monkeypatch):
    _, _, bedrock = fakes
    monkeypatch.setattr(
        profile_loader,
        "load_profile",
        lambda *a, **k: (
            list(profile_loader.DEFAULT_CATEGORIES),
            {"format": "# 独自見出し", "rules": ["時間は30分単位に丸める"]},
        ),
    )
    index.handler({"date": "2026-08-01"}, None)
    system_text = bedrock.requests[0]["system"][0]["text"]
    assert "# 独自見出し" in system_text
    assert "- 時間は30分単位に丸める" in system_text
    assert "サマリ" not in system_text  # the default format is not used


def test_ユーザーメッセージに合計時間の表とセグメントが入る(fakes):
    _, _, bedrock = fakes
    index.handler({"date": "2026-08-01"}, None)
    user_text = bedrock.requests[0]["messages"][0]["content"][0]["text"]
    assert "対象日: 2026-08-01" in user_text
    assert "| コーディング | 5 |" in user_text  # the 5 minutes from 10:00 to 10:05
    assert "| 調べ物・閲覧 | 1 |" in user_text  # 1 minute for the last record
    segment_line = next(
        line for line in user_text.splitlines() if line.startswith("{")
    )
    seg = json.loads(segment_line)
    assert seg == {
        "start": "10:00",
        "end": "10:05",
        "minutes": 5,
        "device": "mac-main",
        "app": "Code",
        "category": "coding",
        "summary": "作業",
        "records": 1,
    }
