# ADR 0002: Vulkan Instanceの初期化・診断・試験

- 日付: 2026-11-12
- 状態: 採用
- 関連課題: `renderer-qhg`、後続 `renderer-7ro`

## 背景

M0のビルド基盤へ、最初のVulkan実装を加える。qhgの対象はInstanceと検証・診断の
作成から解放までであり、Device、VMA、転送、RenderGraph、Presentationは含めない。
通常のCPU検証をVulkan実行環境から独立させつつ、実行時の問題をログで調査できるようにする。

## 採用案

- `vulkan` feature配下のprivate実装で、ashの実行時ロードとVulkan 1.3のInstanceを使う。
  `VK_LAYER_KHRONOS_validation`と`VK_EXT_debug_utils`を必須とし、検証なしモードや
  自動フォールバックは作らない。検証固有の設定・messengerを分離し、後から明示的な
  検証なしモードを追加できる構造にする。公開APIや汎用Backend traitは追加しない。
- 初期化前にAPI、Layer、extensionを照会し、取得できた不足条件をまとめて返す。
  読み込みや照会が失敗した場合は、操作と元の原因を型付きエラーへ保持する。
  照会失敗で不明な条件を「不足」と決めつけない。明示的なLayerが提供するextensionも確認する。
- 一つの所有型がEntry、Instance、messenger、安定したallocation上の`Arc<Diagnostics>`を保持する。
  Instance作成直後からguardに所有させ、後続の失敗でも後始末する。
  messengerを先に破棄し、Instanceを破棄してから診断状態とEntryを解放する。
  `vkCreateInstance`の`pNext`にもコールバック設定を渡し、生成中・破棄中も受信する。
- `tracing`を使い、利用側が初期化前にsubscriber・出力先・表示レベルを設定する。
  ライブラリはglobal subscriberを設定しない。診断状態は開始時の`Dispatch`を保持し、
  別スレッドのコールバックも同じsubscriberへ送る。将来のGraphのspan伝播までは保証しない。
- GENERAL／VALIDATION／PERFORMANCEの全重要度を受信し、ERROR → `error`、WARNING → `warn`、
  INFO → `info`、VERBOSE → `trace`へ対応させる。件数・保持はログの表示設定から独立させる。
  直近64件と最初のwarning／error 128件を保持し、上限後も件数を数え、保持できなかった
  問題メッセージ数を示す。集計は対象操作・解放の完了後に確認する。
- コールバックは文字列を所有データへコピーし、並行呼び出しへ対応する。
  集計ロックを解放してからsubscriberを呼び、Vulkan APIは呼ばず、常に`VK_FALSE`を返す。
  subscriberにもVulkanへ再入しない条件を課す。panicをFFIへ伝播させず、内部失敗を記録する。
  subscriberのpanic後は、その診断状態からの外部ログ転送を停止し、集計は続ける。
  panic payloadの破棄もpanicし得るため、この例外経路ではpayloadを破棄しない。
  `panic=abort`・OOMによるabortからの回復は対象外とする。
- 実Vulkan受入試験はwarning／errorともゼロを要求する。意図的な利用や性能上の助言も
  許容理由にしない。例外は確認済みの誤検出だけで、メッセージID・対象バージョン・条件・根拠を
  個別に記録し、ログへ残す。現時点の例外はない。コールバック内部の失敗も試験失敗とする。

## 比較した案

テスト専用出力だけにすると、利用アプリでの調査経路が不足するため、最初からログも出す。
`log`の代わりに`tracing`を採用し、構造化した識別情報を記録する。
出力設定はアプリ側に残し、Rendererがプロセス全体のログ設定を変更しない。

検証あり／なしの両モード、専用診断コマンド、未使用の公開APIは今は追加しない。
privateな実装は通常のVulkan featureビルドにも含め、試験専用実装にはしない。
通常経路の呼び出し元がない間だけ、bootstrapモジュールに理由を示して`dead_code`を許容する。

## 検証と見直し

通常の`cargo test`は、`--features vulkan`を付けてもVulkanを初期化しない。
要件判定、診断、失敗・解放経路はCPU試験で確認する。試験用のash関数テーブルは
制御フローと所有権の確認用であり、ドライバーやGPUの検証結果には含めない。

実Vulkan試験は`#[ignore]`とし、[開発手順](../development.md)のコマンドで明示実行する。
不足環境を成功やskipとして扱わない。正常な生成・解放の反復、INFOメッセージの受信、
実Instance作成後のホスト側失敗注入と後始末を確認する。注入失敗を実ドライバーのOOM再現とは扱わない。
Instance試験の成功から、GPU選択、Device対応、同期、転送、描画の成功は主張しない。

通常実行の呼び出し元、検証なしモード、別バックエンド、診断の保存・出力要件が必要になった時点で
境界を見直す。実測した検証結果はADRの設計判断と分けて記録する。

## 一次資料

- [ash 0.38 EntryとInstanceの寿命](https://docs.rs/ash/0.38.0+1.3.281/ash/struct.Entry.html)
- [Instance作成・破棄中のdebug callback](https://docs.vulkan.org/refpages/latest/refpages/source/VkInstanceCreateInfo.html)
- [debug callbackの制約](https://docs.vulkan.org/refpages/latest/refpages/source/PFN_vkDebugUtilsMessengerCallbackEXT.html)
- [tracingのsubscriberとDispatch](https://docs.rs/tracing/0.1.44/tracing/dispatcher/index.html)
