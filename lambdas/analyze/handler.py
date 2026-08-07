"""Analyze Lambda. Triggered by an S3 put event; analyzes the image with Bedrock and stores an activity record."""

import json
import os
import re
import urllib.parse
from datetime import datetime, timedelta, timezone

import boto3
from botocore.config import Config
from botocore.exceptions import ClientError

import profile_loader
import prompts

JST = timezone(timedelta(hours=9))
KEY_PATTERN = re.compile(
    r"^raw/(?P<device>[a-z0-9-]{1,32})/\d{4}/\d{2}/\d{2}/\d{6}\.json$"
)
OCR_TEXT_MAX = 500
# Cap on the OCR text embedded in the prompt in local OCR mode (same limit the client applies)
OCR_PROMPT_MAX = 4000

s3 = boto3.client("s3")
dynamodb = boto3.client("dynamodb")
bedrock = boto3.client(
    "bedrock-runtime",
    config=Config(retries={"max_attempts": 5, "mode": "adaptive"}),
)


def _log(**fields):
    print(json.dumps(fields, ensure_ascii=False))


def parse_key(key: str) -> str | None:
    """Return the deviceId from a metadata JSON key, or None if the key does not match the format."""
    m = KEY_PATTERN.match(key)
    return m.group("device") if m else None


def build_pk_sk(captured_at: str, device: str) -> tuple[str, str]:
    """Convert capturedAt (ISO 8601 with offset) to JST and build the pk / sk pair."""
    dt = datetime.fromisoformat(captured_at).astimezone(JST)
    return f"DAY#{dt:%Y-%m-%d}", f"TS#{dt:%H:%M:%S}#{device}"


def validate_category(category: str, categories: list[dict]) -> str:
    if any(c["id"] == category for c in categories):
        return category
    return "other"


def analyze_image(image_bytes: bytes, metadata: dict, categories: list[dict]) -> tuple[dict, dict]:
    """Call Bedrock and return (tool input, usage)."""
    context_text = (
        f"アプリ: {metadata.get('app', '')}\n"
        f"ウィンドウタイトル: {metadata.get('windowTitle', '')}\n"
        f"撮影時刻: {metadata.get('capturedAt', '')}"
    )
    response = bedrock.converse(
        modelId=os.environ["MODEL_ID"],
        system=[{"text": prompts.build_system_prompt(categories)}],
        messages=[
            {
                "role": "user",
                "content": [
                    {"image": {"format": "webp", "source": {"bytes": image_bytes}}},
                    {"text": context_text},
                ],
            }
        ],
        toolConfig=prompts.TOOL_CONFIG,
        inferenceConfig={"maxTokens": 1024, "temperature": 0},
    )
    return parse_tool_response(response)


def analyze_text(metadata: dict, categories: list[dict]) -> tuple[dict, dict]:
    """Local OCR mode. Send no image; classify and summarize from the OCR text alone."""
    context_text = (
        f"アプリ: {metadata.get('app', '')}\n"
        f"ウィンドウタイトル: {metadata.get('windowTitle', '')}\n"
        f"撮影時刻: {metadata.get('capturedAt', '')}\n"
        f"OCR テキスト:\n{metadata.get('ocrText', '')[:OCR_PROMPT_MAX]}"
    )
    response = bedrock.converse(
        modelId=os.environ["MODEL_ID"],
        system=[{"text": prompts.build_text_system_prompt(categories)}],
        messages=[{"role": "user", "content": [{"text": context_text}]}],
        toolConfig=prompts.TEXT_TOOL_CONFIG,
        inferenceConfig={"maxTokens": 1024, "temperature": 0},
    )
    return parse_tool_response(response)


def parse_tool_response(response: dict) -> tuple[dict, dict]:
    if response["stopReason"] != "tool_use":
        raise RuntimeError(f"stopReason が tool_use でない: {response['stopReason']}")
    tool_input = next(
        block["toolUse"]["input"]
        for block in response["output"]["message"]["content"]
        if "toolUse" in block
    )
    return tool_input, response["usage"]


def process_record(key: str) -> None:
    bucket = os.environ["BUCKET_NAME"]
    device = parse_key(key)
    if device is None:
        _log(event="skip_invalid_key", key=key)
        return

    metadata = json.loads(s3.get_object(Bucket=bucket, Key=key)["Body"].read())
    text_mode = "imageKey" not in metadata

    categories, _ = profile_loader.load_profile(
        s3, bucket, os.environ["PROFILE_KEY"]
    )

    try:
        if text_mode:
            # Local OCR mode: no image in S3, the metadata's ocrText is all we have
            result, usage = analyze_text(metadata, categories)
        else:
            image_bytes = s3.get_object(Bucket=bucket, Key=metadata["imageKey"])["Body"].read()
            result, usage = analyze_image(image_bytes, metadata, categories)
    except ClientError as e:
        if e.response["Error"]["Code"] == "ValidationException":
            _log(event="analyze_failed", key=key, error=str(e))
        raise

    ocr_text = metadata.get("ocrText", "") if text_mode else result["ocrText"]
    pk, sk = build_pk_sk(metadata["capturedAt"], device)
    item = {
        "pk": {"S": pk},
        "sk": {"S": sk},
        "device": {"S": metadata["device"]},
        "capturedAt": {"S": metadata["capturedAt"]},
        "app": {"S": metadata.get("app", "")},
        "windowTitle": {"S": metadata.get("windowTitle", "")},
        "category": {"S": validate_category(result["category"], categories)},
        "summary": {"S": result["summary"]},
        "ocrText": {"S": ocr_text[:OCR_TEXT_MAX]},
    }
    if not text_mode:
        item["imageKey"] = {"S": metadata["imageKey"]}
    dynamodb.put_item(TableName=os.environ["TABLE_NAME"], Item=item)
    _log(
        event="analyzed",
        key=key,
        inputTokens=usage["inputTokens"],
        outputTokens=usage["outputTokens"],
    )


def lambda_handler(event, context):
    for record in event.get("Records", []):
        key = urllib.parse.unquote_plus(record["s3"]["object"]["key"])
        if not key.endswith(".json"):
            continue
        process_record(key)
