from handler import build_pk_sk, parse_key


class TestParseKey:
    def test_正常なキーから_deviceId_を取り出す(self):
        assert parse_key("raw/mac-main/2026/08/01/102300.json") == "mac-main"

    def test_プレフィックスが_raw_でないと_None(self):
        assert parse_key("config/mac-main/2026/08/01/102300.json") is None

    def test_deviceId_に大文字が含まれると_None(self):
        assert parse_key("raw/Mac-Main/2026/08/01/102300.json") is None

    def test_時刻部分の桁が足りないと_None(self):
        assert parse_key("raw/mac-main/2026/08/01/1023.json") is None

    def test_拡張子が_webp_だと_None(self):
        assert parse_key("raw/mac-main/2026/08/01/102300.webp") is None

    def test_階層が足りないと_None(self):
        assert parse_key("raw/mac-main/2026/08/102300.json") is None


class TestBuildPkSk:
    def test_JST_の時刻はそのまま使う(self):
        pk, sk = build_pk_sk("2026-08-01T10:23:00+09:00", "mac-main")
        assert pk == "DAY#2026-08-01"
        assert sk == "TS#10:23:00#mac-main"

    def test_UTC_の時刻を_JST_に変換する(self):
        pk, sk = build_pk_sk("2026-08-01T01:23:00+00:00", "mac-main")
        assert pk == "DAY#2026-08-01"
        assert sk == "TS#10:23:00#mac-main"

    def test_JST_変換で日付が繰り上がる(self):
        pk, sk = build_pk_sk("2026-08-01T16:30:00+00:00", "mac-main")
        assert pk == "DAY#2026-08-02"
        assert sk == "TS#01:30:00#mac-main"
