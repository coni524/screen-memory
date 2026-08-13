"""System prompts and tool schemas."""

SYSTEM_PROMPT_TEMPLATE = """あなたはスクリーンショットから作業内容を記録する分類器である。
与えられた画像は利用者のアクティブウィンドウの撮影で、アプリ名とウィンドウタイトルが添えられている。
record_activity ツールで次を報告せよ。
- category: 後述の分類一覧から最も近い id を1つ選ぶ。判断できなければ other。
- summary: 何をしていたかを日本語一文で。画面の固有の内容（ファイル名、ページ名、相手など）を含める。
- ocrText: 画面上の主要なテキストを最大500文字で抜粋する。パスワードやクレジットカード番号らしき文字列は含めない。

分類一覧:
{categories}"""

# Fallback for responses cut off at max_tokens. Does not ask for ocrText
FALLBACK_SYSTEM_PROMPT_TEMPLATE = """あなたはスクリーンショットから作業内容を記録する分類器である。
与えられた画像は利用者のアクティブウィンドウの撮影で、アプリ名とウィンドウタイトルが添えられている。
record_activity ツールで次を報告せよ。
- category: 後述の分類一覧から最も近い id を1つ選ぶ。判断できなければ other。
- summary: 何をしていたかを日本語一文で。画面の固有の内容（ファイル名、ページ名、相手など）を含める。

分類一覧:
{categories}"""

TEXT_SYSTEM_PROMPT_TEMPLATE = """あなたは画面の記録から作業内容を記録する分類器である。
与えられるのは利用者のアクティブウィンドウのアプリ名、ウィンドウタイトル、
および端末のローカル OCR が画面から抽出したテキストである（画像は無い）。
OCR テキストは誤認識や取りこぼしを含むので、アプリ名とウィンドウタイトルも手掛かりにする。
record_activity ツールで次を報告せよ。
- category: 後述の分類一覧から最も近い id を1つ選ぶ。判断できなければ other。
- summary: 何をしていたかを日本語一文で。画面の固有の内容（ファイル名、ページ名、相手など）を含める。

分類一覧:
{categories}"""

TOOL_CONFIG = {
    "tools": [
        {
            "toolSpec": {
                "name": "record_activity",
                "description": "スクリーンショット1枚の作業内容を記録する",
                "inputSchema": {
                    "json": {
                        "type": "object",
                        "properties": {
                            "category": {"type": "string"},
                            "summary": {"type": "string"},
                            "ocrText": {"type": "string"},
                        },
                        "required": ["category", "summary", "ocrText"],
                    }
                },
            }
        }
    ],
    "toolChoice": {"tool": {"name": "record_activity"}},
}

# For text mode (local OCR) and the image-mode fallback. No ocrText in the schema.
TEXT_TOOL_CONFIG = {
    "tools": [
        {
            "toolSpec": {
                "name": "record_activity",
                "description": "画面の記録1件の作業内容を記録する",
                "inputSchema": {
                    "json": {
                        "type": "object",
                        "properties": {
                            "category": {"type": "string"},
                            "summary": {"type": "string"},
                        },
                        "required": ["category", "summary"],
                    }
                },
            }
        }
    ],
    "toolChoice": {"tool": {"name": "record_activity"}},
}


def _category_lines(categories: list[dict]) -> str:
    lines = []
    for c in categories:
        if c.get("criteria"):
            lines.append(f"- {c['id']}: {c['label']}（{c['criteria']}）")
        else:
            lines.append(f"- {c['id']}: {c['label']}")
    return "\n".join(lines)


def build_system_prompt(categories: list[dict]) -> str:
    return SYSTEM_PROMPT_TEMPLATE.format(categories=_category_lines(categories))


def build_text_system_prompt(categories: list[dict]) -> str:
    return TEXT_SYSTEM_PROMPT_TEMPLATE.format(categories=_category_lines(categories))


def build_fallback_system_prompt(categories: list[dict]) -> str:
    return FALLBACK_SYSTEM_PROMPT_TEMPLATE.format(categories=_category_lines(categories))
