"""Read API Lambda. Serves both HTTP API (API Gateway v2) routes from a single function.

API Gateway's JWT authorizer (Cognito) handles authentication, so only verified
requests reach this code.
"""

import json
import os
import re
from decimal import Decimal

import boto3
from boto3.dynamodb.types import TypeDeserializer

DATE_PATTERN = re.compile(r"^\d{4}-\d{2}-\d{2}$")

RECORD_FIELDS = (
    "capturedAt",
    "device",
    "app",
    "windowTitle",
    "category",
    "summary",
    "ocrText",
    "imageKey",
)

s3 = boto3.client("s3")
dynamodb = boto3.client("dynamodb")

_deserializer = TypeDeserializer()


def _log(**fields):
    print(json.dumps(fields, ensure_ascii=False))


def _json_response(status: int, body: dict) -> dict:
    return {
        "statusCode": status,
        "headers": {"content-type": "application/json; charset=utf-8"},
        "body": json.dumps(body, ensure_ascii=False),
    }


def _to_plain(value):
    """Convert DynamoDB Decimal values to int so they can be serialized to JSON."""
    if isinstance(value, Decimal):
        return int(value)
    return value


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
    plain = {k: _to_plain(_deserializer.deserialize(v)) for k, v in item.items()}
    return {k: plain[k] for k in RECORD_FIELDS if k in plain}


def get_records(date: str, query_params: dict) -> dict:
    records = [to_record(item) for item in fetch_records(date)]
    device = query_params.get("device")
    if device:
        records = [r for r in records if r.get("device") == device]
    return _json_response(
        200, {"date": date, "count": len(records), "records": records}
    )


def get_report(date: str, query_params: dict) -> dict:
    yyyy, mm, dd = date.split("-")
    key = f"reports/{yyyy}/{mm}/{dd}.md"
    try:
        response = s3.get_object(Bucket=os.environ["BUCKET_NAME"], Key=key)
    except s3.exceptions.NoSuchKey:
        return _json_response(404, {"message": "report not found"})
    markdown = response["Body"].read().decode("utf-8")
    if query_params.get("format") == "md":
        return {
            "statusCode": 200,
            "headers": {"content-type": "text/markdown; charset=utf-8"},
            "body": markdown,
        }
    return _json_response(200, {"date": date, "markdown": markdown})


ROUTES = {
    "GET /days/{date}/records": get_records,
    "GET /days/{date}/report": get_report,
}


def handler(event, context):
    try:
        route = ROUTES.get(event.get("routeKey"))
        if route is None:
            return _json_response(404, {"message": "not found"})

        date = (event.get("pathParameters") or {}).get("date", "")
        if not DATE_PATTERN.match(date):
            return _json_response(400, {"message": "invalid date"})

        return route(date, event.get("queryStringParameters") or {})
    except Exception as error:  # noqa: BLE001
        _log(event="api_error", error=repr(error))
        return _json_response(500, {"message": "internal error"})
