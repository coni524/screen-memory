Screen Memory has been installed. / Screen Memory のインストールが完了しました。

--- English ---

The app starts automatically at logon (via Task Scheduler) and lives in the
notification area (right end of the taskbar) with a camera icon.

Two one-time steps:

1. Create the config file (only needed for the AWS modes)
   Create %APPDATA%\screen-memory\config.toml with:

     device_id = "win-main"            # lowercase letters, digits, hyphens; up to 32 chars
     bucket = "<S3 bucket name>"
     region = "ap-northeast-1"
     user_pool_id = "<Cognito user pool ID>"
     user_pool_client_id = "<app client ID>"
     identity_pool_id = "<identity pool ID>"
     cognito_domain = "<Hosted UI base URL>"

2. Log in
   Right-click the Screen Memory icon in the notification area and
   choose "Login"; a browser window opens.

To send OCR text instead of images, pick the send-text mode in the same
menu. To keep everything on this machine without AWS, pick the local-only
mode (no config file or login needed).

Uninstalling removes the autostart entry and the app itself, but keeps the
settings (%APPDATA%\screen-memory) and staged data
(%LOCALAPPDATA%\screen-memory). Delete them by hand if you no longer
need them.

--- 日本語 ---

ログオン時にタスクスケジューラ経由で自動起動し、通知領域（タスクバー右端）に
カメラのアイコンで常駐します。

初回のみ、次の2つを行ってください。

1. 設定ファイルを置く（AWS へ送るモードを使う場合のみ）
   %APPDATA%\screen-memory\config.toml を作り、次の内容を書く。

     device_id = "win-main"            # 半角小文字・数字・ハイフンで32文字まで
     bucket = "<S3 バケット名>"
     region = "ap-northeast-1"
     user_pool_id = "<Cognito ユーザープール ID>"
     user_pool_client_id = "<アプリクライアント ID>"
     identity_pool_id = "<ID プール ID>"
     cognito_domain = "<Hosted UI のベース URL>"

2. ログインする
   通知領域の Screen Memory アイコンを右クリックし、メニューの
   「ログイン」を選ぶとブラウザが開く。

画像を送らず OCR テキストだけ AWS に送るときは、同じメニューの
文字送信モードを選ぶ。AWS に何も送らず端末内に記録するだけなら
ローカル保存モードを選ぶ（設定ファイルもログインも不要）。

アンインストールすると自動起動の登録と本体は削除されますが、
設定（%APPDATA%\screen-memory）と記録の一時データ
（%LOCALAPPDATA%\screen-memory）は残ります。不要なら手で削除してください。
