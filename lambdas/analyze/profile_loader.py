"""Fetches the rule profile (config/profile.yaml) and caches it by ETag."""

import json
import time

import yaml
from botocore.exceptions import ClientError

DEFAULT_CATEGORIES = [
    {"id": "coding", "label": "コーディング"},
    {"id": "writing", "label": "文書作成"},
    {"id": "browsing", "label": "調べ物・閲覧"},
    {"id": "meeting", "label": "会議"},
    {"id": "chat", "label": "チャット・メール"},
    {"id": "other", "label": "その他"},
]

CACHE_TTL_SECONDS = 300

# (categories, report_rules, etag, fetched_at). A None etag means the object does not exist
_cache: tuple[list[dict], dict | None, str | None, float] | None = None


def load_profile(s3_client, bucket: str, key: str, *, now: float | None = None):
    """Return the profile as (categories, report_rules)."""
    global _cache
    if now is None:
        now = time.time()

    if _cache is not None:
        categories, report_rules, etag, fetched_at = _cache
        if now - fetched_at < CACHE_TTL_SECONDS:
            return categories, report_rules
        current_etag = _head_etag(s3_client, bucket, key)
        if current_etag == etag:
            _cache = (categories, report_rules, etag, now)
            return categories, report_rules

    categories, report_rules, etag = _fetch(s3_client, bucket, key)
    _cache = (categories, report_rules, etag, now)
    return categories, report_rules


def _head_etag(s3_client, bucket: str, key: str) -> str | None:
    try:
        return s3_client.head_object(Bucket=bucket, Key=key)["ETag"]
    except ClientError as e:
        if e.response["Error"]["Code"] in ("404", "NoSuchKey", "NotFound"):
            return None
        raise


def _fetch(s3_client, bucket: str, key: str):
    try:
        obj = s3_client.get_object(Bucket=bucket, Key=key)
    except ClientError as e:
        if e.response["Error"]["Code"] in ("404", "NoSuchKey", "NotFound"):
            return list(DEFAULT_CATEGORIES), None, None
        raise
    etag = obj["ETag"]
    try:
        profile = yaml.safe_load(obj["Body"].read())
        categories, report_rules = _normalize(profile)
    except Exception as e:
        print(json.dumps({"event": "profile_load_error", "key": key, "error": str(e)}, ensure_ascii=False))
        return list(DEFAULT_CATEGORIES), None, etag
    return categories, report_rules, etag


def _normalize(profile) -> tuple[list[dict], dict | None]:
    if not isinstance(profile, dict):
        raise ValueError(f"プロファイルが辞書でない: {type(profile).__name__}")
    raw = profile.get("categories")
    if raw is None:
        categories = list(DEFAULT_CATEGORIES)
    else:
        categories = []
        for c in raw:
            categories.append(
                {"id": c["id"], "label": c["label"], "criteria": c.get("criteria")}
            )
        if not any(c["id"] == "other" for c in categories):
            categories.append({"id": "other", "label": "その他"})
    report_rules = profile.get("report")
    return categories, report_rules
