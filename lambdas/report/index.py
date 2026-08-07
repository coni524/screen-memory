"""Daily report Lambda. EventBridge Scheduler invokes it once a day to turn the day's activity records into a Markdown report and store it in S3."""

import json
import os
import re
from datetime import datetime, timedelta, timezone

import boto3
from botocore.config import Config

import profile_loader

JST = timezone(timedelta(hours=9))
DATE_PATTERN = re.compile(r"^\d{4}-\d{2}-\d{2}$")

# Cap on the gap to the next record that still counts as working time (excludes idle and stopped periods)
MAX_RECORD_SECONDS = 15 * 60
# Duration attributed to the last record of the day
LAST_RECORD_SECONDS = 60
# Cap on the number of segments included in the prompt
MAX_SEGMENTS = 300

DEFAULT_FORMAT = """\
# 日報 {date}
## サマリ（3行以内）
## 時間配分（分類別の表。合計時間の確定値をそのまま使う）
## 主な作業（時系列で5〜10項目）"""

s3 = boto3.client("s3")
dynamodb = boto3.client("dynamodb")
bedrock = boto3.client(
    "bedrock-runtime",
    config=Config(retries={"max_attempts": 5, "mode": "adaptive"}),
)


def _log(**fields):
    print(json.dumps(fields, ensure_ascii=False))


def resolve_date(event) -> str:
    """Determine the target date from the event. Defaults to today in JST; raises ValueError on a malformed date."""
    raw = (event or {}).get("date")
    if raw is None:
        return f"{datetime.now(JST):%Y-%m-%d}"
    if not isinstance(raw, str) or not DATE_PATTERN.match(raw):
        raise ValueError(f"date の形式が不正: {raw!r}")
    try:
        datetime.strptime(raw, "%Y-%m-%d")
    except ValueError:
        raise ValueError(f"date が実在しない日付: {raw!r}")
    return raw


def fetch_records(date: str) -> list[dict]:
    kwargs = {
        "TableName": os.environ["TABLE_NAME"],
        "KeyConditionExpression": "pk = :pk AND begins_with(sk, :ts)",
        "ExpressionAttributeValues": {
            ":pk": {"S": f"DAY#{date}"},
            ":ts": {"S": "TS#"},
        },
    }
    items = []
    while True:
        response = dynamodb.query(**kwargs)
        items.extend(response["Items"])
        last_key = response.get("LastEvaluatedKey")
        if not last_key:
            return items
        kwargs["ExclusiveStartKey"] = last_key


def to_record(item: dict) -> dict:
    """Convert a DynamoDB item into a record used for segment calculation."""
    time_part = item["sk"]["S"].split("#")[1]  # hh:mm:ss
    h, m, s = (int(x) for x in time_part.split(":"))
    return {
        "seconds": h * 3600 + m * 60 + s,
        "device": item.get("device", {}).get("S", ""),
        "app": item.get("app", {}).get("S", ""),
        "category": item.get("category", {}).get("S", "other"),
        "summary": item.get("summary", {}).get("S", ""),
    }


def build_segments(records: list[dict]) -> list[dict]:
    """Aggregate the change-point records into segments of continuous time."""
    records = sorted(records, key=lambda r: r["seconds"])
    segments = []
    for i, record in enumerate(records):
        if i + 1 < len(records):
            duration = min(
                records[i + 1]["seconds"] - record["seconds"], MAX_RECORD_SECONDS
            )
        else:
            duration = LAST_RECORD_SECONDS
        prev = segments[-1] if segments else None
        if prev is not None and all(
            prev[k] == record[k] for k in ("device", "category", "app")
        ):
            prev["end_seconds"] = record["seconds"] + duration
            prev["duration"] += duration
            prev["records"] += 1
        else:
            segments.append(
                {
                    "start_seconds": record["seconds"],
                    "end_seconds": record["seconds"] + duration,
                    "duration": duration,
                    "device": record["device"],
                    "app": record["app"],
                    "category": record["category"],
                    "summary": record["summary"],
                    "records": 1,
                }
            )
    return segments


def _hhmm(seconds: int) -> str:
    return f"{seconds // 3600:02d}:{seconds % 3600 // 60:02d}"


def to_segment_json(segment: dict) -> dict:
    return {
        "start": _hhmm(segment["start_seconds"]),
        "end": _hhmm(segment["end_seconds"]),
        "minutes": max(1, round(segment["duration"] / 60)),
        "device": segment["device"],
        "app": segment["app"],
        "category": segment["category"],
        "summary": segment["summary"],
        "records": segment["records"],
    }


def category_totals(segments: list[dict]) -> dict[str, int]:
    """Total minutes per category. Sums the already-rounded values so the totals match the segments' minutes."""
    totals: dict[str, int] = {}
    for seg in segments:
        totals[seg["category"]] = totals.get(seg["category"], 0) + seg["minutes"]
    return totals


def thin_segments(segments: list[dict]) -> tuple[list[dict], int]:
    """Drop the shortest segments once there are more than 300. Returns (kept, dropped count)."""
    if len(segments) <= MAX_SEGMENTS:
        return segments, 0
    order = sorted(
        range(len(segments)), key=lambda i: (-segments[i]["minutes"], i)
    )
    keep = sorted(order[:MAX_SEGMENTS])
    return [segments[i] for i in keep], len(segments) - MAX_SEGMENTS


def build_system_prompt(report_rules, date: str) -> str:
    fmt = None
    rules = None
    if isinstance(report_rules, dict):
        fmt = report_rules.get("format")
        rules = report_rules.get("rules")
    lines = [
        "あなたは1日の作業記録から日報を書くアシスタントである。",
        "与えられるのは分類別の合計時間（確定値。変更しない）と、時系列のセグメント一覧である。",
        "日本語で、Markdown の日報だけを出力する。前置きや後書きは書かない。",
        "見出し構成は次のフォーマットに従う。",
        fmt.strip() if fmt else DEFAULT_FORMAT.format(date=date),
    ]
    if rules:
        lines.append("追加ルール：")
        lines.extend(f"- {rule}" for rule in rules)
    return "\n".join(lines)


def build_user_message(
    date: str,
    totals: dict[str, int],
    labels: dict[str, str],
    segments: list[dict],
    dropped: int,
) -> str:
    lines = [
        f"対象日: {date}",
        "",
        "分類別の合計時間（確定値）:",
        "| 分類 | 合計(分) |",
        "|---|---|",
    ]
    for category, minutes in sorted(totals.items(), key=lambda kv: -kv[1]):
        lines.append(f"| {labels.get(category, category)} | {minutes} |")
    lines += ["", "セグメント一覧（時系列、1行1件の JSON）:"]
    lines += [json.dumps(seg, ensure_ascii=False) for seg in segments]
    if dropped:
        lines.append(
            f"（注：セグメントが {MAX_SEGMENTS} 件を超えたため、minutes の小さい順に {dropped} 件を間引いてある）"
        )
    return "\n".join(lines)


def generate_report(system_prompt: str, user_message: str) -> tuple[str, dict]:
    response = bedrock.converse(
        modelId=os.environ["MODEL_ID"],
        system=[{"text": system_prompt}],
        messages=[{"role": "user", "content": [{"text": user_message}]}],
        inferenceConfig={"maxTokens": 4096, "temperature": 0.3},
    )
    text = "".join(
        block["text"]
        for block in response["output"]["message"]["content"]
        if "text" in block
    )
    return text, response["usage"]


def save_report(date: str, markdown: str) -> str:
    yyyy, mm, dd = date.split("-")
    key = f"reports/{yyyy}/{mm}/{dd}.md"
    s3.put_object(
        Bucket=os.environ["BUCKET_NAME"],
        Key=key,
        Body=markdown.encode(),
        ContentType="text/markdown; charset=utf-8",
    )
    dynamodb.put_item(
        TableName=os.environ["TABLE_NAME"],
        Item={
            "pk": {"S": f"DAY#{date}"},
            "sk": {"S": "REPORT"},
            "reportKey": {"S": key},
            "generatedAt": {"S": datetime.now(JST).isoformat(timespec="seconds")},
        },
    )
    return key


def handler(event, context):
    date = resolve_date(event)
    items = fetch_records(date)
    if not items:
        _log(event="report_skipped", date=date)
        return {"date": date, "skipped": True}

    segments = [to_segment_json(s) for s in build_segments([to_record(i) for i in items])]
    totals = category_totals(segments)
    segments, dropped = thin_segments(segments)

    categories, report_rules = profile_loader.load_profile(
        s3, os.environ["BUCKET_NAME"], os.environ["PROFILE_KEY"]
    )
    labels = {c["id"]: c["label"] for c in categories}

    markdown, usage = generate_report(
        build_system_prompt(report_rules, date),
        build_user_message(date, totals, labels, segments, dropped),
    )
    if not markdown.strip():
        raise RuntimeError("日報の生成結果が空だった")

    key = save_report(date, markdown)
    _log(
        event="report_generated",
        date=date,
        key=key,
        inputTokens=usage["inputTokens"],
        outputTokens=usage["outputTokens"],
    )
    return {"date": date, "reportKey": key}
