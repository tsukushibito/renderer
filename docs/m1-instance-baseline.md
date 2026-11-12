# M1先頭課題: Instance初期化・診断の確認結果

確認日: 2026-11-12。対象課題: `renderer-qhg`。
M1全体ではなく、Instanceと検証・診断の初期化から解放までの記録である。
設計判断は[ADR 0002](adr/0002-vulkan-instance-bootstrap.md)、
再現コマンドは[開発手順](development.md)に記載する。

## 実装した範囲

privateなVulkanバックエンドに、ashの実行時ロード、Vulkan 1.3の要求、
必須Layer／extensionの照会、Instance、debug messenger、型付きエラーを追加した。
照会できた不足条件をまとめて返し、照会失敗の操作・原因も保持する。
明示Layerが提供するextensionも照会対象に含める。

診断は`tracing`へ発行し、ログ設定とは独立してwarning／errorを集計する。
作成・破棄中のコールバックも受信し、正常終了と部分初期化失敗の後始末を所有型へ集約した。
検証なしモード、公開API、Device、VMAリソース、転送、Graph、表示は追加していない。

## 実行環境

| 項目 | 今回の確認 |
|---|---|
| OS / target | Windows x64、プロセス報告 `10.0.26200.0` / `x86_64-pc-windows-msvc` |
| Rust | rustc 1.99.0 (`b940084d7`) |
| Instance API | 1.3.0を明示要求。ローダーの問い合わせ結果は1.4.341 |
| Validation Layer | SDK 1.4.341.1の`VK_LAYER_KHRONOS_validation`。manifestのAPIは1.4.341、implementation versionは1 |
| debug extension | `VK_EXT_debug_utils`を明示有効化 |
| ログ依存 | tracing 0.1.44、テスト出力用tracing-subscriber 0.3.23 |
| GPU / Device | この課題では選択・作成しない |

## 実行した確認

| 確認 | 結果 |
|---|---|
| `cargo fmt --all -- --check` | 成功 |
| workspace / all-targets / no-default-featuresのcheckとclippy (`-D warnings`) | 成功 |
| 同じ構成の通常test | 成功。テスト0件、Vulkanを初期化しない |
| 上記check / clippy / testへ`--features vulkan`を追加 | 成功。CPU上の14件成功、実Vulkanの2件はignored |
| 明示実行 `vulkan_instance_smoke` | 成功。Instance／messengerの生成・解放とINFOメッセージ受信を4回確認 |
| 明示実行 `vulkan_instance_partial_failure` | 成功。実Instance作成後、messenger作成前のホスト側失敗注入・解放を4回確認 |
| 実Vulkan試験の終了後集計 | 各反復でwarning 0、error 0、callback内部失敗0。例外登録なし |
| `cargo tree --locked --no-default-features --edges normal` | rendererのみ。通常のCPUライブラリにGPU・ログ依存はない |
| `git diff --check` | 成功 |

明示実行コマンド:

```sh
cargo test --locked -p renderer --no-default-features --features vulkan backends::vulkan::tests::vulkan_instance_ -- --ignored --nocapture
```

CPU試験は不足条件の集約、原因の保持、診断の所有データ化・重要度対応・並行呼び出し、
ログ無効時の集計、保持上限、subscriberのpanic、診断状態の異常を確認した。
試験用ash関数テーブルでは、Instance作成失敗・messenger作成失敗・entry point不足時の
解放回数と順序、Instance破棄中のコールバック状態の生存を、実装の制御フローを通して確認した。

warning／errorはVulkanが渡すseverityで判定する。作成状態通知には
`WARNING-CreateInstance-status-message`というIDのINFOも含まれたが、
warningの例外扱い・抑制は行っていない。

## 未検証の範囲と次の作業

この記録の実行結果はWindowsでのローカル確認であり、Linuxでのローカル実行、別環境、リモートCIの結果は含めない。
Windows／Linuxの既存CI構成は維持し、リモートCIの結果はGitHub Actionsで別途確認する。
CPUの関数テーブル試験を実ドライバーの検証とは扱わず、失敗注入をドライバーの実OOM再現とは扱わない。

Instance試験の成功は、GPUの能力照会、Device作成、GPU同期、転送、描画の検証を意味しない。
Synchronization Validationの明示設定や性能計測も今回の対象外である。
次は`renderer-7ro`で、Physical Deviceの能力照会とDevice・単一キューを実装する。
