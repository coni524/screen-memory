import json

import pytest

import handler
import profile_loader
from handler import validate_category

CATEGORIES = [{"id": "coding", "label": "コーディング"}, {"id": "other", "label": "その他"}]


class TestValidateCategory:
    def test_一覧にある_id_はそのまま(self):
        assert validate_category("coding", CATEGORIES) == "coding"

    def test_一覧に無い_id_は_other_に落とす(self):
        assert validate_category("hacking", CATEGORIES) == "other"


class FakeBody:
    def __init__(self, data: bytes):
        self._data = data

    def read(self):
        return self._data


METADATA = {
    "device": "mac-main",
    "capturedAt": "2026-08-01T10:23:00+09:00",
    "app": "Visual Studio Code",
    "windowTitle": "DESIGN.md — screen-memory",
    "imageKey": "raw/mac-main/2026/08/01/102300.webp",
}

# Metadata for local OCR mode: no imageKey, but it carries ocrText
TEXT_METADATA = {
    "device": "mac-main",
    "capturedAt": "2026-08-01T10:23:00+09:00",
    "app": "Visual Studio Code",
    "windowTitle": "DESIGN.md — screen-memory",
    "ocrText": "画面から抽出したテキスト",
}


class FakeS3:
    def __init__(self, metadata=METADATA):
        self.metadata = metadata
        self.image_requests = 0

    def get_object(self, Bucket, Key):
        if Key.endswith(".json"):
            return {"Body": FakeBody(json.dumps(self.metadata).encode())}
        self.image_requests += 1
        return {"Body": FakeBody(b"webp-bytes")}


class FakeDynamoDB:
    def __init__(self):
        self.items = []

    def put_item(self, TableName, Item):
        self.items.append(Item)


class FakeBedrock:
    def __init__(self, tool_input):
        self.tool_input = tool_input
        self.requests = []

    def converse(self, **kwargs):
        self.requests.append(kwargs)
        return {
            "stopReason": "tool_use",
            "output": {"message": {"content": [{"toolUse": {"input": self.tool_input}}]}},
            "usage": {"inputTokens": 1000, "outputTokens": 100},
        }


@pytest.fixture
def fakes(monkeypatch):
    monkeypatch.setattr(profile_loader, "load_profile", lambda *a, **k: (list(profile_loader.DEFAULT_CATEGORIES), None))
    dynamodb = FakeDynamoDB()
    bedrock = FakeBedrock({"category": "coding", "summary": "設計文書を編集していた。", "ocrText": "DESIGN.md"})
    monkeypatch.setattr(handler, "s3", FakeS3())
    monkeypatch.setattr(handler, "dynamodb", dynamodb)
    monkeypatch.setattr(handler, "bedrock", bedrock)
    return dynamodb, bedrock


def s3_event(key):
    return {"Records": [{"s3": {"object": {"key": key}}}]}


def test_正常系で活動レコードを保存する(fakes):
    dynamodb, bedrock = fakes
    handler.lambda_handler(s3_event("raw/mac-main/2026/08/01/102300.json"), None)
    assert len(dynamodb.items) == 1
    item = dynamodb.items[0]
    assert item["pk"] == {"S": "DAY#2026-08-01"}
    assert item["sk"] == {"S": "TS#10:23:00#mac-main"}
    assert item["category"] == {"S": "coding"}
    assert item["imageKey"] == {"S": METADATA["imageKey"]}
    assert bedrock.requests[0]["inferenceConfig"] == {"maxTokens": 1024, "temperature": 0}


def test_webp_のイベントは無視する(fakes):
    dynamodb, _ = fakes
    handler.lambda_handler(s3_event("raw/mac-main/2026/08/01/102300.webp"), None)
    assert dynamodb.items == []


def test_形式不一致のキーは警告して飛ばす(fakes, capsys):
    dynamodb, _ = fakes
    handler.lambda_handler(s3_event("raw/BAD/2026/08/01/102300.json"), None)
    assert dynamodb.items == []
    assert "skip_invalid_key" in capsys.readouterr().out


def test_URL_エンコードされたキーをデコードする(fakes):
    dynamodb, _ = fakes
    handler.lambda_handler(s3_event("raw%2Fmac-main%2F2026%2F08%2F01%2F102300.json"), None)
    assert len(dynamodb.items) == 1


def test_ocrText_が500文字を超えたら切り詰める(fakes):
    dynamodb, bedrock = fakes
    bedrock.tool_input = {"category": "coding", "summary": "s", "ocrText": "あ" * 600}
    handler.lambda_handler(s3_event("raw/mac-main/2026/08/01/102300.json"), None)
    assert len(dynamodb.items[0]["ocrText"]["S"]) == 500


class TestTextMode:
    @pytest.fixture
    def text_fakes(self, fakes, monkeypatch):
        dynamodb, bedrock = fakes
        bedrock.tool_input = {"category": "coding", "summary": "設計文書を編集していた。"}
        s3 = FakeS3(TEXT_METADATA)
        monkeypatch.setattr(handler, "s3", s3)
        return dynamodb, bedrock, s3

    def test_imageKey_が無ければ画像を取得せずテキストで解析する(self, text_fakes):
        dynamodb, bedrock, s3 = text_fakes
        handler.lambda_handler(s3_event("raw/mac-main/2026/08/01/102300.json"), None)
        assert s3.image_requests == 0
        item = dynamodb.items[0]
        assert item["category"] == {"S": "coding"}
        assert item["ocrText"] == {"S": TEXT_METADATA["ocrText"]}
        assert "imageKey" not in item
        request = bedrock.requests[0]
        content = request["messages"][0]["content"]
        assert all("image" not in block for block in content)
        assert TEXT_METADATA["ocrText"] in content[0]["text"]
        assert request["toolConfig"]["tools"][0]["toolSpec"]["inputSchema"]["json"]["required"] == ["category", "summary"]

    def test_長い_ocrText_はプロンプトも保存も切り詰める(self, text_fakes, monkeypatch):
        dynamodb, bedrock, s3 = text_fakes
        s3.metadata = dict(TEXT_METADATA, ocrText="あ" * 5000)
        handler.lambda_handler(s3_event("raw/mac-main/2026/08/01/102300.json"), None)
        prompt_text = bedrock.requests[0]["messages"][0]["content"][0]["text"]
        assert prompt_text.count("あ") == handler.OCR_PROMPT_MAX
        assert len(dynamodb.items[0]["ocrText"]["S"]) == handler.OCR_TEXT_MAX


def test_stopReason_が_tool_use_でなければ例外(fakes):
    _, bedrock = fakes

    def bad_converse(**kwargs):
        return {"stopReason": "max_tokens", "output": {"message": {"content": []}}, "usage": {}}

    bedrock.converse = bad_converse
    with pytest.raises(RuntimeError):
        handler.lambda_handler(s3_event("raw/mac-main/2026/08/01/102300.json"), None)
