# NFC Bridge

NFC リーダーのカード UID を読み取り、ローカル WebSocket サーバー経由でブラウザに配信する Windows サービス。

## アーキテクチャ

```
[NFC Reader] --PC/SC API--> [nfc-bridge] --WebSocket--> [Browser]
                              ws://localhost:9876
```

## インストール (Windows)

### MSI インストーラー

1. [Releases](https://github.com/yhonda-ohishi-alc/rust-nfc-bridge/releases) から最新の `.msi` をダウンロード
2. MSI を実行してインストール
3. NFC Bridge サービスが自動的に起動します

インストール先: `C:\Program Files\NfcBridge\`

### サービス管理

```powershell
# サービス状態確認
sc query NfcBridge

# サービス停止
sc stop NfcBridge

# サービス開始
sc start NfcBridge

# サービス再起動 (設定変更後)
sc stop NfcBridge && sc start NfcBridge
```

### 設定ファイル

`C:\Program Files\NfcBridge\nfc-bridge.toml` を編集:

```toml
port = 9876
bind_addr = "127.0.0.1"
poll_interval_ms = 200
cooldown_ms = 3000
log_dir = ""
```

設定変更後はサービスを再起動してください。

### ログ

`C:\Program Files\NfcBridge\logs\nfc-bridge.log` にデイリーローテーションで出力されます。

## コンソールモード (開発・デバッグ)

```bash
# コンソールモードで実行
nfc-bridge.exe --console

# カスタム設定
nfc-bridge.exe --console --port 8765 --poll-interval-ms 100

# デバッグログ
set RUST_LOG=nfc_bridge=debug
nfc-bridge.exe --console

# 設定ファイル指定
nfc-bridge.exe --console --config path/to/config.toml
```

## 通信仕様

ブラウザが `ws://localhost:9876` に WebSocket 接続し、JSON メッセージを受信する。

### NFC 読み取り成功

```json
{ "type": "nfc_read", "employee_id": "AABBCCDD" }
```

### NFC エラー

```json
{ "type": "nfc_error", "error": "no_readers" }
```

### ステータス

```json
{ "type": "status", "readers": ["ACS ACR122U"], "connected": true }
```

## 対応カード

- MIFARE (UID: 4/7 バイト)
- FeliCa (IDm: 8 バイト) — 社員証等

GET DATA APDU (`FF CA 00 00 00`) でカード UID を抽出。PIN 不要。

## 前提条件

- **Windows**: `winscard.dll` が組み込み済み (追加インストール不要)
- PC/SC 対応 NFC リーダー (ACR122U, Sony RC-S380 等)

## 開発

### ビルド

```bash
cargo build --release
```

### テスト

```bash
cargo test
```

### MSI ビルド (Windows)

```bash
cargo install cargo-wix
cargo build --release
cargo wix --no-build
```

## 技術スタック

- `tokio` - 非同期ランタイム
- `pcsc` - PC/SC スマートカード API
- `tokio-tungstenite` - WebSocket サーバー
- `windows-service` - Windows Service API
- `clap` - CLI 引数パーサー
- `tracing` - 構造化ログ
- `toml` - 設定ファイル
