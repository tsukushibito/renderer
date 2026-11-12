# ADR 0003: Vulkan Device・自動選択・単一キュー

- 日付: 2026-11-12
- 状態: 採用
- 関連課題: `renderer-7ro`、後続 `renderer-yr6`

## 背景

[Instance基盤](0002-vulkan-instance-bootstrap.md)の上に、デバイスの能力照会、
Deviceとキューの作成・解放を加える。M1全体の転送・readbackへ進む前に、
必須機能の対応確認と有効化、所有権、実行環境の診断を確認できる区切りを設ける。

## 採用案

- `vulkan` feature配下のprivate実装を維持する。物理デバイスのAPI variantが0かつ
  Vulkan 1.3以上であること、`timelineSemaphore`、`synchronization2`、
  `dynamicRendering`、Graphics＋Compute対応キューを照会する。
  照会していない機能を「不足」と判断しない。
- 対応確認とDevice作成時の有効化を分ける。作成用のfeature構造体を新たに組み立て、
  必要な3 featureだけを有効化する。照会で返されたfeature全体をそのまま有効化しない。
- 適合候補を専用GPU、内蔵GPU、仮想GPU、その他、CPU型の順に優先する。
  同順位は列挙順で選ぶ。今回は自動選択だけを実装し、GPU指定の公開設定は追加しない。
  CPU型も最後の候補として利用するが、その選択をログに残す。
- 必須機能の不足による候補除外と、選択後の作成失敗を分ける。
  適合候補がなければ各デバイスの不足理由をまとめた型付きエラーを返す。
  列挙などのAPI呼び出し失敗は操作と元の`vk::Result`を保持する。
  選択したデバイスの`vkCreateDevice`が失敗した場合は、そのデバイスと原因を報告して停止し、
  別デバイスへ自動的に切り替えない。
- `queueCount > 0`でGraphics＋Computeに対応する最初のqueue familyを選ぶ。
  queue indexは0、取得数は1、priorityは1.0とする。選択したfamily indexを保持し、
  固定のfamily番号を前提にしない。キューはDeviceとともに作成・破棄される。
- 候補と除外理由、選択理由、デバイス名・種別・API、vendor／device ID、
  driver名・追加情報・ID・生のバージョン値、queue family／indexを診断へ残す。
  `driverVersion`はドライバー依存の値として保持し、汎用のAPIバージョンとして分解しない。
- Device所有型が`VulkanInstance`を値で所有する。Device作成成功直後から所有型で管理し、
  Device、debug messenger、Instanceの順に解放する。
  今回はsubmitもDeviceの子リソースもなく、解放前に待つGPU作業がないため、
  `vkDeviceWaitIdle`は追加しない。コマンド投入の導入時には、完了確認と再利用・破棄の責任を実装する。
- `vulkan.rs`をモジュールの入口とし、既存Instance実装を`vulkan/instance.rs`へ移す。
  Device実装は`vulkan/device.rs`に置く。既存の分離済みInstance試験はInstance配下へ移し、
  新規unit testは原則として実装ファイル末尾に置く。
  公開API、汎用Backend trait、buffer／image、submit、RenderGraph、表示、複数キューは追加しない。

## 比較した案

初期から複数キューを用意すると、処理の割り当て、キュー間同期、必要なfamily間所有権移譲、
複数キューの完了を覆うリソース寿命管理も必要になる。
まず単一キューを基準にし、処理が揃った段階で性能を比較する。
そのため、キュー選択・投入・完了管理の責任をまとめ、将来の変更が各所へ広がることを避ける。
未使用のキュー設定やスケジューラーは先行実装しない。

GPU指定、同順位候補の性能スコア、自動的な作成再試行は今回の基盤に必須ではない。
同じ優先度の選択結果が再起動や環境変更を越えて固定される保証は設けない。
CPU型の候補を認めても、CPU上での成功を実GPU検証の代替とは扱わない。

## 検証と見直し

通常の`cargo test`はVulkanを初期化しない。
CPU試験で不足条件と選択規則、実際のDevice作成設定に含まれる必須3 featureと単一キュー、
作成失敗時の後始末と解放順序を確認する。
必要な外部境界だけを試験用に置き換え、Vulkan全体を模した実装は作らない。

実Vulkan試験は`#[ignore]`とし、[開発手順](../development.md)のコマンドで明示実行する。
実GPU受入試験はCPU型を選択した場合に失敗し、ソフトウェア実装上の成功で代替しない。
作成・解放を反復し、DeviceだけでなくInstanceの解放後まで集計する。
warning、error、診断コールバック内部失敗はゼロを要求する。
例外は[ADR 0002](0002-vulkan-instance-bootstrap.md)の確認済み誤検出に限り、ログにも残す。
実行条件不足をskipや成功に置き換えない。

実際に選ばれたデバイス・driver・API・キュー・有効featureと結果は、
[Device・単一キューの確認結果](../m1-device-baseline.md)に記録する。
Device試験の成功からGPU転送・同期・描画の成功は主張しない。
複数キュー、表示、Device子リソース、コマンド投入、GPU指定が必要になった時点で境界を見直す。

## 一次資料

- [featureの照会とDevice作成時の有効化](https://docs.vulkan.org/refpages/latest/refpages/source/VkPhysicalDeviceFeatures2.html)
- [キューの能力と作成・取得](https://docs.vulkan.org/guide/latest/queues.html)
- [ドライバー識別情報](https://docs.vulkan.org/refpages/latest/refpages/source/VkPhysicalDeviceDriverProperties.html)
- [Deviceの破棄条件](https://docs.vulkan.org/refpages/latest/refpages/source/vkDestroyDevice.html)
