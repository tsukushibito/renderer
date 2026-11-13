# ゲームエンジン向け3D Renderer 実装方針

**対象:** Codexおよび実装担当者  
**作成日:** 2026-11-12  
**文書バージョン:** 2.0

**実装言語・対象API:** Rust / Vulkan / D3D12（実行基盤はB0で再評価）
**設計の中心:** RenderGraphによる描画処理の宣言・解析・実行

> 到達目標は、Bistroをテクスチャ付きPBR・影・線形HDRとトーンマッピングで描画し、利用側の独自shaderによる多段Bloomを追加・交換できる、ゲームエンジン向けRendererである。
> 同じ利用側のパス定義と入力をVulkanとD3D12で実行する。実シーンと利用コードから公開APIを設計し、採用基盤を選び、機能を縦切りで実証する。

本書は今後の方針であり、実装済みの範囲は各設計文書と検証結果で区別する。既存基盤の再利用は必須にしない。Bistro到達までの進めやすさ、総工数、公開APIのUX、品質と保守負担から、維持・改修・置換・外部基盤採用を比較して選ぶ。既存の検証実績は判断材料であり、構造を維持する理由そのものにはしない。

本文の「必須」は守るべき契約、「初期推奨」は実装対象と検証環境に応じて変更できる出発点、「後続」は初期実装の完了条件には含めない事項を示す。重要な変更は、理由・代替案を現在の設計文書へ、検証結果を現在の検証状況へ反映する。置き換え済みの判断はGit履歴から参照する。

## 1. 最重要の実装判断

| 項目 | 方針 |
|---|---|
| GPU処理の標準的な投入経路 | RenderGraphとする。標準描画機能もカスタムパスも、原則として同じ経路を使う。 |
| 採用基盤 | 現行の`ash`・`vk-mem`による自前実装も含め、改修・置換・外部基盤利用をB0で比較する。自前実装を目的にしない。 |
| 資源割当・同期・寿命 | Rendererとして保証する責任を明確にし、採用基盤へ委譲する範囲と自前で実装する範囲を決める。既存allocator等を利用し、用途のない自作を増やさない。 |
| Graphとパスの境界 | パス作者が処理と資源アクセスを定義し、基盤がフレーム全体の依存・資源・実行を管理する。新しい効果の追加でGraph本体を編集しない。 |
| RHIの扱い | 代表的な利用コードから必要な契約を設計する。型名や共通コマンド体系の形を先に固定せず、二APIの実行で確認する。 |
| 初期の実行経路 | 単一の描画・Compute用キュー、エイリアシングなし、保守的で正しい同期を基準とする。 |
| 二APIの実証 | VulkanとWindows上のD3D12を対象とする。同じ論理Graph・パス定義・入力で実証し、描画機能をbackendごとに二重実装する公開設計にしない。 |
| 公開API | エンジン向けAPIとGraph APIへ、`ash::vk`や`vk_mem`の型を露出させない。 |
| 最適化の判断 | 出力の正しさとCPU時間・GPU時間・メモリ使用量の計測で判断する。薄い抽象化だけで高速化を保証しない。 |

従来の`wgpu`不採用と自前の同期・物理配置を優先する選択も、B0の比較対象へ戻す。外部基盤の採用は未決定であり、二API、独自パス、所有・寿命、診断、製品費用を満たすか採用versionの一次資料と小試作で確認する。物理aliasingや複数queueの直接制御を、Bistro初期描画の一律要件にしない。

## 2. 最終目標と対象範囲

### 2.1 ゲームエンジンへ提供するもの

Rendererは特定のECS、アセット管理システム、ウィンドウイベントループに依存しないライブラリとして設計する。エンジンはシーンの所有と更新を担当し、Rendererへ描画に必要なデータを渡す。

Renderer内には、Mesh・Material等をモデル単位に集約するModel構造を含めない。
利用側のモデル表現をRenderer独自の構造へ合わせる制約を避けるためである。
この方針はRendererの責務境界を定めるものであり、利用側のモデル管理方式やレイヤー構成は規定しない。

| 領域 | 最終的に満たすべき内容 |
|---|---|
| 3D描画 | メッシュ、マテリアル、テクスチャ、カメラ、ライトを扱い、深度を含む実用的な描画を行う。 |
| 基本的な描画品質 | PBR、シャドウ、HDRの内部描画、トーンマッピング、透明描画などを段階的に整備する。方式は実装時に検証して決める。 |
| エンジン統合 | リソースの作成・更新・破棄、複数View、オフスクリーン出力、フレーム間履歴、アップロード・readbackを明確な契約で扱う。 |
| 拡張性 | エンジン側からカスタム描画・Compute・コピー処理をグラフへ追加できる。 |
| 実シーンでの成立 | Bistroをテクスチャ付きPBR・影・線形HDR・トーンマッピングで描画する。独自Bloomの追加・無効化・交換をRenderer外の利用コードで行う。 |
| クロスプラットフォーム | Vulkanに加えて異なるネイティブAPIを実装し、公開契約の移植性を実証する。 |
| 品質保証 | 画像回帰、GPU検証、性能回帰、リソース寿命テスト、再現可能なサンプルを提供する。 |
| 調査可能性 | ユーザーのパスから、実際のリソース・バリア・キュー投入まで追跡できる。 |

初期対象はPCネイティブを想定する。最終的なOS・GPU世代・必須機能・画質目標・性能予算は、検証機材と用途を踏まえて記録する。未計測の「商用エンジンと同等性能」や、未検証のプラットフォーム対応を成果として扱わない。

### 2.2 初期の対象外

ゲームエンジン全体、ECS、物理、アニメーションシステム全体、エディター、アセット制作環境は作らない。サンプル用の最小ローダーやシーンは許可するが、Rendererの必須依存へ拡大しない。

汎用GPUライブラリとしての独立RHI、全バックエンドの同時完成、レイトレーシング、Mesh Shader、GPU-driven描画、全面的なバインドレス化、複雑な仮想ジオメトリなどは初期要件にしない。Web・モバイル・コンソール対応も別途要件化するまで実装対象に含めない。

これらを将来可能にすることと、未使用の抽象化や機能を先回りして実装することは区別する。

## 3. 現行の依存ライブラリと採用基盤の再評価

### 3.1 現行のashとvk-memの役割

以下は現行実装の構成であり、新計画での維持義務ではない。B0の採用判断までは現行の契約を適用し、置換時は移行先の契約と検証方法を更新する。

`ash`はVulkanに近いバインディングであり、独自のRenderGraphやGPU寿命管理を提供する層ではない。`vk.xml`から生成された型と関数を使い、Vulkanバックエンドの処理を記述する。[^ash]

`vk-mem`はVMAのRustバインディングであり、メモリタイプ選択、メモリブロックの確保・サブアロケーション、マッピングなどに利用する。グラフに基づく同期・寿命・キュー割り当てはRendererが担当する。[^vkmem]

作成時に確認した組み合わせは、`ash 0.38.0+1.3.281`と`vk-mem 0.5.0`である。`vk-mem 0.5.0`のmanifestは`ash 0.38`系を指定している。[^ashdocs][^vkmemmanifest]

```toml
[dependencies]
ash = "0.38"
vk-mem = "0.5"
```

これは開始用の互換バージョン指定であり、将来にわたって「最新」であると仮定しない。既存manifestがある場合は、その依存関係を確認してから変更する。実際の解決バージョンは`Cargo.lock`で管理し、更新は独立した検証対象にする。

### 3.2 ビルド環境

`vk-mem`は同梱のC++コードをビルドするため、対象環境のC++ツールチェーンを用意する。Rustだけで完結する依存と考えない。コンパイラー、ターゲット、ネイティブ依存の準備をCIと開発手順に記載する。[^vmbuild]

Vulkanローダー、対応ドライバー、検証レイヤーの有無は、ビルド可否と分けて確認する。SDK・バインディングに定義があるだけでGPUが対応するとは判断せず、使用するfeatureは問い合わせと有効化を行う。[^features]

Rustのedition、MSRV、シェーダーコンパイラー、ウィンドウ接続用クレートは既存環境との整合を見て決定する。外部ツールに依存する場合はバージョンと再現手順を固定し、開発者個人の環境にだけ存在する生成物へ依存しない。

## 4. 設計の中心は「グラフを実行する基盤」

### 4.1 論理的な責務分担

以下は責務の区分であり、この数だけクレートや抽象レイヤーを作る指示ではない。

| 責務 | 内容 |
|---|---|
| Engine-facing API | メッシュ・マテリアル・View・描画入力・出力など、エンジンが利用する契約。 |
| Graph API | パス、リソースアクセス、依存、出力、副作用、カスタム処理の宣言。 |
| 共通解析 | 依存解析、不要パス除去、使用範囲・論理寿命の解析、宣言の検証。 |
| バックエンドを考慮する計画 | キュー、同期、一時メモリ、記録単位を対象APIの制約に合わせて決める。 |
| パス内の命令記録 | パイプライン・バインディング・draw・dispatch・copyなどを扱う。 |
| 長寿命ランタイム | デバイス、永続リソース、キャッシュ、GPU完了、遅延破棄、複数グラフ間の状態を管理する。 |
| Presentation | Surface、Swapchain、acquire、present、再作成、表示側の同期を扱う。 |

共通解析だけで全実行計画を固定せず、メモリ要件、キュー能力、フォーマット制約などをバックエンドから計画部へ返せるようにする。参考例としてAtomは、共通のFrameGraphコンパイル処理と、API固有の同期・実行への具体化を分けている。本書はその開発順序やクラス構成を再現する指示ではない。[^atom]

### 4.2 初期の配置例

```text
renderer/
  api/                 # エンジン向けの意味論・ハンドル・エラー
  graph/               # 宣言、中間データ、共通解析、グラフのテスト
  runtime/             # フレーム、永続リソース、完了、遅延破棄
  backends/
    vulkan/            # ash・vk-memを使用する実装
      device/
      resources/
      memory/
      graph_plan/
      commands/
      sync/
  present/             # 表示との境界。Vulkan固有部は明確に分離
  shaders/             # シェーダーの契約・生成・キャッシュとの接続
  diagnostics/         # グラフダンプ、識別子、計測
examples/
tests/
docs/
  guides/              # 現行の開発・利用手順
  design/              # 現在有効な設計と判断理由
  verification/current.md
```

既存の`renderer-core`、`renderer-present`等があれば、責務を対応づけて利用する。グラフをcoreへ置くか別クレートへ置くかは、依存方向とテストの必要性で決める。名称を合わせるためだけの大規模移動は行わない。

必須条件は、グラフの純粋な解析テストがGPU・ウィンドウなしで実行できること、通常の公開APIがVulkanやVMAの型へ依存しないこと、Presentationのために解析部分へSwapchain管理を持ち込まないことである。

## 5. RHIを抽出する進め方

### 5.1 最初に固定しないもの

Vulkan・D3D12・Metalの全操作を列挙した巨大な`Backend`トレイト、共通コマンドバッファの全仕様、全拡張を表す列挙型、独立利用可能なRHIクレートは先に作らない。

B0で自前基盤を選ぶ場合、Vulkanバックエンド内でashを直接使ってよい。外部基盤を選ぶ場合は、その公開契約と委譲範囲を記録する。少数の検証用パスにネイティブな命令記録が必要な場合も、実験用・バックエンド内部の経路として局所化する。

### 5.2 最初から守る境界

論理リソースと物理リソース、アクセスの意味とVulkan状態、グラフの寿命とGPU完了、グラフ構築と命令実行は分ける。ネイティブハンドルを通常のGraph APIの識別子として使わない。

境界を保つための最小限の型・関数・内部インターフェースは作ってよい。「RHIを後から抽出する」ことを理由に、無制限な直接依存を許さない。

### 5.3 第二バックエンドで確認すること

第二バックエンドはVulkanと異なるAPIから選ぶ。M3でWindows上のD3D12を選定した。
実機・CI・対象OS・保守担当を判断材料とした理由と見直し条件は[16.1節](#161-第二バックエンドの選定と準備条件)に記す。

同じ論理グラフ、同等のシェーダー処理、同じ出力条件を動かしながら、次を分類する。

| 分類 | 方針 |
|---|---|
| グラフの意味として共通 | 宣言・依存解析・不要パス除去などを共有する。 |
| GPU操作の契約として共通 | リソース記述、パス内操作、完了確認などを必要な範囲で抽出する。 |
| プラットフォーム固有 | バリアの表現、メモリ配置制約、キュー実装などをバックエンドへ残す。 |

二つ目のRenderGraph全体をコピーして別系統にしない。一方、少数の実験パスで命令記録が重複することは許容する。共通化するのは似たコードではなく、同じ前提と保証を提供する契約である。

Vulkanバックエンドを別OSで動かすことは移植テストであり、異なるGPU APIへの抽象化を検証したことにはならない。MoltenVK経由の検証だけで、ネイティブMetalバックエンドの契約を検証済みとしない。

### 5.4 能力ベースの設計

共通の基本契約と任意機能を分ける。全APIの共通部分だけに制限しない一方、全APIの全機能を初期から実装することも目標にしない。

「利用可能」「実装済み」「有効化済み」「今回の計画で採用」は区別する。未対応機能は明示的にエラーまたはフォールバックへ進め、成功したように見せて何もしない実装を置かない。

## 6. RenderGraphの必須契約

### 6.1 宣言する情報

各パスは、デバッグ名、処理種別、使用リソース、アクセス範囲、読み書きの用途、入力・出力を宣言する。画像はmip・layer・aspect、バッファはbyte範囲を扱える意味論にする。

初期実装で範囲を粗く扱う場合は、安全側に全体として追跡してよい。ただし、未実装の細粒度追跡を提供しているように見せない。サポート外の指定は拒否するか、保守的な扱いを診断情報へ示す。

Attachmentのload/store、clear、discardも依存と内容の有効性に関わる契約として扱う。常にwrite-onlyと仮定せず、既存内容の保持が必要な操作を区別する。

### 6.2 依存解析と内容の有効性

RAW（書き込み後の読み込み）、WAR（読み込み後の書き込み）、WAW（書き込み後の書き込み）を識別する。単にパスをトポロジカルソートするだけで同期が成立したことにしない。

リソースのバージョン化、明示的なproducer-consumer関係、またはそれと同等の規則で「どの書き込みの結果を読むか」を決める。複数書き込みが曖昧な場合はエラーにする。構築順を意味のある規則として採用するなら、その制約を文書化する。

未初期化の読み込み、範囲外アクセス、未定義リソース、循環依存を検出する。`read_write`と宣言しただけで初期値が存在するとは扱わない。初回使用時にclearまたは完全な初期化を必要とするか、外部から有効な内容を受け取る契約が必要である。

### 6.3 不要パス除去と副作用

最終出力、export、readback、外部に観測される書き込み、副作用を起点として必要なパスを残す。外部リソースへの書き込みを、グラフ内に後続の読み込みがないという理由だけで削除しない。

実行コールバックは、不要パス除去で呼ばれない場合がある。コールバック内の任意のCPU処理を、毎フレーム必ず実行されるゲーム更新処理として使わせない。副作用を持つ処理には明示的な契約を設ける。

### 6.4 一つのパス内の同期

パス単位のread/write宣言だけで、複数dispatch間の依存まで自動的に解決できると約束しない。初期実装では、同期が必要な処理を別ノードへ分割する。後に内部同期APIを追加する場合は、グラフの保証範囲との境界を定義する。Vulkanでも、先行dispatchの書き込みを後続処理で読む場合には必要な依存を設定する。[^sync]

一つのグラフノードを、一つのネイティブRenderPassや一つのCommandBufferと恒久的に一対一対応させない。物理的な記録単位は計画側の判断として残す。

### 6.5 import / exportとグラフ外の状態

永続リソースの通常のimportでは、ランタイムが把握した識別情報・既存内容・前回の使用状態・未完了処理を引き継ぐ。同じ物理リソースやそのViewを複数回importしても、別物と誤認してハザードを見落とさない。

ネイティブ外部リソースの取り込みでは、所有権、初期状態、有効な内容、完了条件、返却時の条件を別契約として明示する。すべてを無条件に「初期状態は不明だからdiscard」で処理しない。

exportは、永続化または外部へ渡す意味と、利用開始条件を定める。CPUがexportハンドルを受け取った時点でGPU処理が完了したとは扱わない。複数グラフやフレーム間の依存もランタイムで引き継ぐ。

### 6.6 バインドレスとネイティブ拡張

将来バインドレスを導入する場合、アクセスし得るリソース集合、テーブル更新、descriptorの寿命を追跡する。GPU側で決まる参照先を、グラフが自動的に知っているとは仮定しない。

ネイティブ拡張は、使用リソース・入出力状態・同期・所有権を明示する限定経路とする。通常の安全なAPIから無制限な生ハンドル操作へ抜けられる設計にしない。保証できない操作には明確な`unsafe`契約または対応外という境界を設ける。

## 7. パス内操作と公開API

通常のカスタムパスには、宣言したリソースだけを利用してpipeline・binding・draw・dispatch等を定義できるAPIを提供する。固定shaderやeffect列挙の選択だけを、カスタムパス対応の完成とは扱わない。構築時に所有された命令列を作る方式、実行callback方式、採用基盤のbuilder等をB0の利用例で比較し、型名や記録時期を先に固定しない。

API設計の検証範囲は広く取り、実装は段階化する。複数draw、depth-only、color-only、独自Compute、多段処理の部品化、履歴import/export、将来の複数描画先・mip範囲を利用例として確認する。すべての機能を初回に実装する指示ではない。

UXの受入条件は、入出力と処理を近くで読めること、同じ資源情報を不必要に重複記述しないこと、通常作者へnative barrier・submitや不要なclone・複雑な寿命指定を要求しないこと、誤りを型・パス名・資源名で判断できることとする。Rustの所有・安全性とGPU寿命の保証を満たしたうえで評価する。

| 通常のパス作者が扱うもの | 実行基盤が扱うもの |
|---|---|
| 使用するパイプライン、バインディング、viewport、draw / dispatch / copy | パス間バリア、物理メモリ配置、キューへのsubmit、GPU完了と破棄 |
| 入力と出力、必要なリソース範囲 | 実行順序、一時リソースの実体化、不要パス除去 |

グラフの規則と、別の汎用RHIのsubmit・状態遷移規則を、通常の利用者へ二重に要求しない。内部にRHIができても、その全機能を公開しない。

パスの宣言と実行時のバインディングを対応づけ、宣言漏れやアクセス用途の不一致を可能な範囲で検証する。制約を満たす通常APIは安全に提供し、検証できないネイティブ操作やシェーダーの取り込みを同じ保証に混ぜない。

ユーザー向けに生のグラフ全体を自由に書き換えさせることだけを、拡張性の目標にしない。例えば「トーンマッピング前のHDRカラーへ処理を追加する」といった、安定した挿入点と入出力の意味を提供する。

その際、色空間、解像度、サンプル数、深度・法線の表現、有効なタイミング、必要な描画設定を明記する。利用できない入力を要求された場合は、診断可能なエラーまたは明示的な代替経路にする。

## 8. リソース所有権とGPU寿命

### 8.1 寿命を分ける

少なくとも次の寿命を混同しない。

| 寿命 | 意味 |
|---|---|
| CPUハンドルの寿命 | エンジンやグラフ構築コードが参照を保持する期間。 |
| 論理リソースの寿命 | グラフ上で必要になる使用区間。 |
| GPUオブジェクトの寿命 | 記録済み・実行中の命令などから参照され得る期間。 |
| 物理メモリの寿命 | バインドされたリソースやGPUアクセスに必要な期間。 |
| 履歴リソースの寿命 | 複数フレームにまたがって内容を維持する期間。 |

永続ハンドルには、型・世代・所有するRendererまたはDeviceの一致を検証できる仕組みを持たせる。古いハンドルや別Deviceのハンドルを、偶然同じ整数値だから有効と扱わない。

グラフローカルの一時ハンドルは、別のグラフや次のフレームで無条件に使用できない契約にする。GPUに必要な参照は、submit時点だけでなく、命令記録からsubmitまでの期間にも保持する。記録失敗・未submit・キャンセル時の解放も設計する。

### 8.2 GPU完了と遅延破棄

リソースの削除要求と、ネイティブオブジェクトの実際の破棄を分ける。最後のCPU参照が消えただけでGPU使用中のリソースを破棄しない。

初期の単一キューでは、FenceまたはTimeline Semaphore等による完了管理を、実装対象に合わせて選ぶ。将来の複数キューでは、そのリソースを使ったすべての未完了処理を覆う完了条件を持たせる。異なるキューの数値を単純に最大値で比較する設計にしない。

command pool、descriptor pool、upload領域、query pool、フレーム一時領域も同じ原則で再利用する。通常フレームごとの`device_wait_idle`やリソースごとの待機で問題を隠さない。初期化・終了・例外的な再作成で待機する場合は、その範囲と理由を明示する。

基本的な再利用は、それを使う実行経路の導入時に整える。B1では採用基盤のCommandPool・CommandBuffer・Fence等を完了後に再利用し、複数の未完了処理を保持できる形にする。上限と空き不足時の動作を明示し、無制限な確保や暗黙のGPU待機を通常経路にしない。descriptorやフレーム一時資源も、導入時からその寿命に合う保持・回収を設計する。

### 8.3 破棄順序と失敗経路

所有する子オブジェクト・Allocationを適切に解放してからAllocator、Device、Instanceを破棄する。`vk-mem::Allocator`の存続中は、その作成に使用したVulkanオブジェクトが有効である必要がある。[^allocator]

部分初期化の失敗、シェーダー作成失敗、メモリ不足、Device lossでも、二重解放や通常経路への誤復帰を起こさない。Device loss後にGPU完了を永久に待ち続けることを避け、終了または再初期化が必要な状態として伝える。自動復旧の完全実装は初期要件にしない。

## 9. vk-mem / VMAを使うメモリ管理

この章のVMA固有の実装選択と第10章のnative実行指針は、自前のVulkan/VMA基盤を直接管理する場合に適用する。
B0で外部基盤を選ぶ場合は、採用基盤が保証する範囲とRenderer側に残る責任を確認して更新する。
割当・転送・同期・寿命の必要な保証は維持するが、VMA Virtual allocator等の採用そのものは必須にしない。

### 9.1 初期は個別Allocationでよい

初期はbuffer/imageごとにAllocationを持たせる実装で開始する。この段階では物理エイリアシングを行わない。個別Allocationは、リソースごとに独立した`vkAllocateMemory`を行うことと同じではなく、VMAのサブアロケーションを利用できる。[^vkmem]

ただし、公開契約や永続的な型設計に「画像とメモリは必ず一対一所有」という前提を埋め込まない。将来、複数の画像が一つのAllocationを共有できるよう、画像の識別とメモリ所有の概念は分ける。

### 9.2 エイリアシングはグラフとバックエンドの責任

VMAは重なるメモリ領域へリソースをバインドする構成を支援するが、グラフの使用期間を自動解析しない。`CAN_ALIAS`等のフラグを設定するだけで最適化が成立したとは扱わない。[^vmaalias]

導入時には、メモリ要件・アラインメント・メモリタイプ・dedicated allocationの条件・画像作成条件を確認する。使用の重なり、切り替え時の依存、初期化の要否を検証する。詳細は採用バージョンのVMAとVulkan仕様に従う。[^vmaalias]

複数キューでは、単一のトポロジカル順序上の区間が離れていてもGPU実行が重なる可能性を考慮する。必要な完了順序を実行計画で証明できない組み合わせは共有しない。追加同期によるメモリ削減と並列性低下を、両方計測する。

共有Allocationの所有者を一つに定め、各画像の破棄時に共有メモリまで繰り返し解放しない。`destroy_image`は画像破棄とAllocation解放を組み合わせる補助関数であるため、共有構成では同じAllocationへ安易に複数回使わない。[^allocator]

### 9.3 CPUからの転送

アップロードとreadbackは、staging領域の再利用条件、GPU完了、アクセス範囲を追跡する。非coherentメモリでは適切なflush / invalidateを行う。mapできることとGPUとの同期が不要であることを混同しない。[^allocator]

高頻度の転送へ接続する前に、capacityを持つbufferのoffset・sizeで複数copyを扱い、完了後に領域を再利用できるようにする。配置にはVMA Virtual allocatorを利用し、GPU完了やCPUによるreadback利用終了の判定をRendererが担う。VMAの既定メモリpoolと、作成済みbuffer内の領域管理は別の層である。

VMAが所有するメモリを直接bind / mapする必要がある場合は、VMA側の同期契約と競合しないかを確認する。通常は対応するVMAのAPIを利用する。[^allocator]

内部断片化、確保済みブロック、利用中のAllocation、一時リソースのピークを可能な範囲で区別して記録する。「論理リソースの合計サイズ」だけをVRAM使用量として報告しない。

## 10. Vulkanバックエンドの初期方針

### 10.1 APIと機能の選定

新規のPC向け実装では、Vulkan 1.3、Dynamic Rendering、Synchronization2を初期推奨とする。ただし、これは最終製品の必須環境を確定するものではない。使用する機材と既存コードを確認して現在の設計文書へ反映する。

必要なバージョン・拡張・featureが不足する環境では、明確な診断を返す。古いAPIへの複数の互換経路を最初から大量に作らない。対応範囲を広げる場合は、要求と検証環境を用意して別タスクにする。featureは対応確認とDevice作成時の有効化を分けて扱う。[^features]

### 10.2 初期実行経路

描画とComputeを実行できるキューを選び、初期はそこへ処理を集約する。present対応キューが別の場合は、それに必要な同期・所有権移譲を実装するか、初期対応外として明示する。「描画キュー一つ」と「presentも必ず同じキュー」は同一条件ではない。

パス間のアクセスから、Vulkanのステージ、アクセスマスク、画像レイアウト、必要な所有権移譲を導く。read-after-readでメモリ依存が不要な場合でも、レイアウトや所有権の条件は別途確認する。同期の削減は基準経路の正しさを確立してから行う。[^sync]

複数キューは後続の最適化として導入する。キューが複数あっても実機で並行実行・高速化が保証されるわけではないため、単一キューの経路を残し、対象GPUで比較する。[^queues]

### 10.3 ホットパスの方針

初期は最適化のための複雑さより、所有権と実行順序の明確さを優先する。ただし、毎drawの汎用ロック、毎フレームの無制限な確保、不要な同期待ちが恒久的な前提にならないよう計測する。

動的ディスパッチ、enumによる分岐、ジェネリクスの選択は実装の明瞭さと測定結果で決める。「薄いRHI」を理由に、未計測のコストを削るためだけの複雑な型システムを導入しない。

## 11. Shader・Pipeline・Bindingの契約

Vulkanバックエンドの初期出力形式はSPIR-Vとする。ソース言語、コンパイラー、変換経路は、第二バックエンドへの展開も考慮して決める。最初にシェーダーシステム全体を完成させる必要はないが、以下の意味論は描画パスを増やす前に記録する。

| 決めるもの | 最低限の内容 |
|---|---|
| バインディングの対応 | 論理的なbindingの識別、リソース種別、配列数、stageからの可視性。 |
| CPU / GPUのデータ配置 | アラインメント、padding、構造体サイズ、行列の規約。Rustの既定の構造体配置を暗黙のGPU ABIにしない。 |
| 描画の座標規約 | handedness、clip/depth範囲、Y方向、front face、行列の扱い、必要な変換。 |
| 色と出力 | linear / sRGB / HDRの扱い、トーンマッピングと表示変換の責任範囲。 |
| Pipelineの互換性 | attachment形式、サンプル数、binding layout、シェーダーバリアント等の識別。 |
| キャッシュ | シェーダー内容、コンパイラー設定、binding規約、対象バックエンドを含む無効化条件。 |

最初は通常のdescriptor set等でよく、全面的なバインドレス化を前提にしない。リソース更新中のdescriptor・pipelineのGPU寿命も追跡する。hot reloadは後続でよいが、古いpipelineをGPU使用中に破棄しない契約を維持する。

pipeline、binding layout、sampler、ImageViewなどは、それぞれを導入する段階から同じ条件のものを保持・再利用する。毎draw・毎フレームでの再作成を基本動作にしない。キャッシュの容量や破棄方針、永続化の程度は実際の利用と測定結果に合わせる。

リフレクションや明示メタデータを用いて、パスの宣言、binding、shaderが要求する型・用途を照合する。任意の未信頼シェーダーを安全に実行するサンドボックスは初期目標ではない。受け入れるシェーダーの信頼境界と、ホスト側で保証できる範囲を文書化する。

## 12. エンジン統合とPresentation

### 12.1 エンジンとのデータ境界

エンジンはシーンとアセットの意味を所有し、RendererはGPUリソースと描画実行を所有する。mesh / material / texture等は安定したRendererハンドルで参照する。

CPU assetの同一性とimmutableな版はバックエンドから独立させる。Vulkanと第二APIへ同じsnapshotを渡すために、
assetを二重登録する設計にしない。GPU常駐はCPU asset寿命と分け、件数・byte予算、退避、準備済みframeのpin、
pending参照の保持を契約にする。大きなuploadは固定容量のstaging内で進め、通常frameに暗黙の待機を加えない。

フレームの入力は、レンダリング処理に必要な期間だけ有効なsnapshot、draw list、更新コマンド等として渡す。GPU処理や遅延実行コールバックが、エンジンの一時的なECS借用や可変シーンへ依存し続けない設計にする。

通常のアプリ開発者へは高水準API、描画拡張の作者へはGraph API、特殊な外部連携へは明示的なinterop契約を提供する。すべての利用者へバックエンド内部の知識を要求しない。

### 12.2 Presentationの独立性

グラフの基本動作はウィンドウなしでも検証できるようにする。最終出力はオフスクリーン画像またはimportされた表示用画像とし、coreがイベントループを所有しない。

Surface・Swapchainの作成、acquire、present、リサイズ、最小化、終了処理はPresentationの責務にする。ただし、必要なDevice・キュー選択や同期情報はランタイムと共有し、互いに同じGPUオブジェクトを独立所有しない。

Vulkanのpresent待機Semaphoreは、単なる描画submitの完了と同じ条件で再利用できると仮定しない。初期は公式ガイドにある画像ごとのpresent待機Semaphoreの方式を参考にし、acquire用・frame slot用の同期と区別する。[^wsi]

Swapchain再作成・破棄時は、旧画像とpresent操作の完了条件も確認する。通常のGPU作業の完了トークンだけで表示側の寿命まで証明したと扱わない。対応拡張の利用や明示的な完了追跡を含め、採用する手順を検証する。[^wsirecreate]

frame slot indexとSwapchain image indexは別概念とする。ゼロサイズ、`OUT_OF_DATE`、`SUBOPTIMAL`、surface loss等を、再試行・再作成・停止のどれに分類するか明示する。

## 13. Rustとunsafeの実装規則

ashを使っていてもGPUの同期や寿命は自動保証されない。Vulkanを呼ぶための`unsafe`を隠すだけで、安全な抽象化が成立したとは判断しない。[^ash]

`unsafe`はバックエンドや明確なinterop境界へ局所化し、各箇所に、その呼び出しが安全である理由と責任の所在をコメントする。単に「呼び出し元が正しく使う」とだけ書かず、寿命、範囲、同期、ハンドルの由来、スレッド条件を具体化する。

`Send` / `Sync`を手動実装する場合は、Vulkan・VMAのhost側同期要件まで確認する。特にCommandPoolやQueueの共有操作について、Rustの型が共有可能でも同時呼び出しが許可されるとは仮定しない。[^queues]

通常の入力エラーや未対応機能は型付きエラーとして返し、公開APIの通常経路で`panic!`・`unwrap()`へ依存しない。FFIコールバックからpanicを外部へ伝播させない。バックエンドのエラー原因、パス、リソース、操作を失わないエラー連鎖にする。

安全なAPIの保証が実行時検証に依存する場合、その検証をreleaseで外しても安全性が維持される根拠が必要である。診断用チェックと安全性を成立させるチェックを分ける。

製品版と開発用の分離には[検証方針](../guides/validation-policy.md)を適用する。
製品のdraw／submitでは保証済みの内容を再走査せず、保証をcook／取込・作成／更新・Graph構築等の境界へ移す。
privateな不変データ・所有権・失効管理で保証できる照合も省略できる。開発用の追加検証は専用情報の保持まで
製品構成から除外する一方、実行計画、同期、GPU完了・寿命、容量とnative失敗の処理は維持する。
これは今後の実装・変更を拘束する方針であり、現在の内部検証を除去済みとは扱わない。

## 14. 診断・テスト・性能計測

### 14.1 最初から出す診断情報

```text
パス名 / 定義位置
  → リソースID / View / アクセス範囲 / バージョン
  → 依存関係 / 除去理由 / 実行順序
  → 物理リソース / Allocation / キュー / submit
  → バリア / wait・signal / 完了条件
```

決定的に比較できるグラフダンプと計画ダンプを用意する。Vulkanオブジェクト名とパスのdebug labelを対応づける。`VK_EXT_debug_utils`はオブジェクト名、ラベル、検証メッセージの受信に利用できる。[^debug]

詳細dump・命名・label・profilingは検証と独立に選択し、製品に不要な構築・保持・文字列化を常設しない。
診断拡張を製品の恒久的な必須条件にする方針ではない。現在のdebug_utils必須という実装状態は
[Vulkan設計](vulkan.md#検証と診断)と[検証方針の差分](../guides/validation-policy.md#current)を参照する。

将来の最適化には、エイリアシング無効、複数キュー無効、並列記録無効などの切り分けスイッチを併設する。ただし、意味を誤ったアクセス宣言まで自動修正する仕組みにしない。

### 14.2 テストを三つに分ける

| 種類 | 内容 | 実行環境 |
|---|---|---|
| CPU単体・性質テスト | グラフの依存、循環検出、不要パス、内容の初期化、範囲の重なり、importの同一性、計画の再現性。 | GPUなしで実行。 |
| GPU統合・画像テスト | 描画、Compute連携、copy/readback、複数フレーム、遅延破棄、リサイズ、異なるバックエンドでの同等出力。 | 実機の対応バックエンド。 |
| 性能テスト | グラフ構築・解析、記録・submit、パス別GPU時間、メモリ使用量、フレーム時間分布。 | 条件を固定した実機。 |

GPUテストではValidation Layerを基本とし、対応環境ではSynchronization Validationも利用する。検証を通ることだけを正しさの証明にしない。Validationのwarning・errorはともにゼロを要求する。例外は確認済みの誤検出だけとし、メッセージID・対象バージョン・発生条件・根拠を個別に記録し、ログにも残す。意図的な利用や性能上の助言は許容理由にせず、広範囲な抑制で隠さない。[^validation]

画像比較は単純シーンの厳密比較と、浮動小数点差を考慮した許容誤差を使い分ける。seed・カメラ・解像度・描画条件を固定し、変更した基準画像には変更理由を添える。

### 14.3 性能の扱い

CPUのgraph build / compile / record / submitと、GPU各パスを分けて測る。GPUのquery結果を即時取得するために毎フレーム待機して、通常実行の性質を変えない。

GPU名、ドライバー、OS、ビルド設定、解像度、機能設定、検証レイヤーの状態、測定回数、warm-upを保存する。検証有効の正しさテストと、最適化ビルドの性能テストを区別する。

性能比較には明示的な検証なし設定を用意し、releaseビルドと固定負荷で測定する。既定の正しさ試験は検証有効を維持する。機能の要求設定だけでなく外部設定の影響も確認し、ログを非表示にしただけで検証なしと扱わない。

最適化と検証は独立の設定軸とする。製品構成では開発用検証を外し、最適化済みの正しさ確認では検証を有効にできるようにする。
切替を実装・変更した場合は両経路を確認し、製品相当測定と計装された測定を区別する。
具体的な分類・feature確認・受入は[検証方針](../guides/validation-policy.md#builds)に従う。

FPSの平均だけで判断せず、フレーム時間の中央値・上位percentile、VRAM、一時メモリピーク、CPU割り当ても比較する。性能予算は代表シーンと対象機材を決めてから設定する。根拠のない固定数値や改善率を作らない。

他エンジンとの比較は補助資料とし、解像度、描画機能、アセット、シェーダー条件等の差を記録する。異なる条件のFPSだけで優劣を断定しない。

<a id="bistro-roadmap"></a>

## 15. Bistroを到達目標にした実装計画と完了条件

親課題は`renderer-7ft`。旧M0〜M4の固定操作による実証順序をB0〜B5へ置き換える。
旧M4は準備・Device・buffer実行の6/22課題まで実証済みで、残りを実装完了とは扱わない。
実績は[現在の検証状況](../verification/current.md)に残し、旧課題は後継を示して置換する。
既存コード、Graphの内部表現、ash・vk-mem、crate構成の維持は必須にしない。

RenderGraphの目的は、描画機能を独立して追加・合成でき、フレーム全体の情報で資源・同期・実行を管理できることである。
DAGや固定操作が動くことと、拡張性の実証を区別する。
[元のFrameGraph講演](https://gdcvault.com/play/1024612/FrameGraph-Extensible-Rendering-Architecture-in)と
[RenderGraphガイド](../guides/rendergraph-best-practices.md#passes)を設計根拠にする。

### 到達対象と共通の受入

素材は[Amazon Lumberyard Bistroの公式配布](https://developer.nvidia.com/orca/amazon-lumberyard-bistro)を基準とする。
公式配布はFBX/Falcor形式で、取得・変換・Model/scene管理は利用側のsample/asset層へ置く。
版・hash・屋内/屋外の最終対象・変換toolと設定・camera/lights・解像度・材質機能はB0で固定する。
部分シーンは初期実証に使えるが、最終対象を黙って縮小したりmesh/materialを省略したりしない。
出典・licenseと再現可能な取得/変換手順を記録し、大容量素材を既定でGitへ追加しない。

| 観点 | 到達条件 |
|---|---|
| 描画 | 対象Bistroをテクスチャ付きPBR・影・線形HDR・トーンマッピングで描く。normal/tangent/UV、色空間、sampling/mip、alphaの解釈と対応範囲を記す。 |
| 拡張性 | 利用側の別module/crateが独自shader/pipelineの多段Bloomを追加・無効化・交換できる。Renderer本体やeffect列挙型の編集を必要としない。 |
| 二API | 同じ論理Graph・利用側パス定義・asset入力・描画条件をVulkan/D3D12実GPUへ渡し、Windowsでは両APIで表示・自由カメラ操作を行える。 |
| 正しさ | 画像・差分を保存し、浮動小数点やsamplingの差に対する比較条件を根拠付きで定める。bufferのbit一致を画像へ機械的に適用しない。 |
| 所有と検証 | 作成・更新・import/export・pending・終了・失敗を各段階で確認する。厳格native検証と診断なし最適化経路を分け、検証方針を満たす。 |
| 性能 | 初回読込と定常描画を分け、CPU build/compile/record/submit・GPU時間・メモリ・uploadを記録する。対象機材・画質と実測から予算を決め、未測定のFPSを約束しない。 |

path-traced参照画像との一致、レイトレーシング/GI、native HDR表示、skinning等を自動的な必須条件にはしない。
静的Bistroだけでは確認できない履歴や動的更新は、小さな補助sceneで検証する。
表示の初期対象は現行のWindowsとする。ユーザーとの要件合意により、最終受入は両APIの画面表示・操作と、
同条件のoffscreen画像比較の両方を要求する。表示経路の実装方式はB0の基盤選定で比較する。

<a id="bistro-requirements"></a>

### Bistroの確定要件と素材検証

`renderer-7ft.1.1`で合意した要件を次に固定する。下の描画方式と準備手順は出発案であり、
素材の実体検証は[検証結果と入力hash](../verification/bistro-assets.json)へ記録した。
描画方式・API基盤の採用決定や、Renderer実装・実GPU受入の完了は意味しない。

| 項目 | 確定した要件 |
|---|---|
| 最終対象 | Bistroの屋外全体。屋内全体は含めない。部分シーンでの着手後も屋外の対象mesh/materialを黙って省略しない。 |
| 描画品質 | テクスチャ付きPBR、太陽光＋空からの光（IBL）＋影、線形HDR・トーンマッピング、利用側独自Bloom。建物・地面間の光の跳ね返りを解く追加GIは必須にしない。IBL自体は間接照明を含む。 |
| 基準解像度 | 1920×1080。画像比較・性能測定の共通条件であり、実行時の解像度変更を妨げない。 |
| カメラ | マウス・キーボードによる自由移動に加え、比較用の固定視点と再現可能な自動巡回。カメラ・入力・シーン管理は利用側に置く。 |
| 表示 | 同じサンプルをVulkan/D3D12のどちらで起動しても、Windowsの画面で表示・操作できる。offscreen画像比較も行う。 |
| 初回受入機材 | 既存Windows x64／RTX 3060環境。既存記録は12 GiBであり、実行時にadapter・driver・能力を再記録する。他GPUでの実機受入を初回の必須条件にはせず、NVIDIA専用設計も要求しない。 |
| 性能予算 | 同じ素材・画質・機材の実測から定める。未測定のFPS、VRAM内への収容、他APIへの優位を約束しない。 |

実行中のAPI切替、二APIの同時二画面表示、カメラの衝突判定・歩行物理は今回の要件に含めない。
native HDR表示、レイトレーシング、追加GI、skinningも上の完成条件には追加しない。

#### 素材と変換の確認仕様

[NVIDIAの公式配布](https://developer.nvidia.com/orca/amazon-lumberyard-bistro)はFBX/Falcor形式、
CC-BY 4.0、屋外2,832,120三角形と記載する。これは配布案内の値であり、取得ファイルの検証値ではない。
引用情報の2017年7月を配布versionとみなさず、取得日時・実ファイル・size・SHA-256で配布物を識別する。
同梱licenseと出典を確認し、作者・配布元・licenseへのリンク、変換した旨を利用側のクレジットへ残す。

| 記録対象 | 取得・変換時に確定する内容 |
|---|---|
| 幾何 | source node path、mesh/instance、transform、三角形、material slot、position/normal/tangent/UV/colorの有無と対応。座標系・単位・面の向き・接線の符号も記録する。 |
| 材質 | base color、metal/roughまたはspec/gloss、normal、emissive、AO、opacity/cutoff、両面、UV set/transform。存在と意味を同梱scene・textureと照合する。 |
| 画像・sampling | 相対path、hash、寸法、channelと色空間、wrap/filter、mipと生成条件、欠落参照。色textureとnormal等のdata textureの変換を分ける。 |
| alpha | opaque/mask/blendの分類と根拠、shadowへの適用。必要な透過表現を不透明へ黙って置き換えず、alpha blendと物理的な透過・屈折を同一視しない。 |
| 変換の再現 | toolの正確なpatch/build・exporter版、script/設定/hash、入力・出力hash、sourceから出力へのnode/instance/material/texture対応。 |

検証用の中間形式にはBlender **4.5.16 LTS / build 3f6797307da5**のheadless実行と
[同梱Khronos exporter](https://github.com/KhronosGroup/glTF-Blender-IO)によるglTF 2.0 Separateを使った。
runtime依存の採用ではない。[BlenderのglTF資料](https://docs.blender.org/manual/en/4.5/addons/import_export/scene_gltf2.html)にある認識可能なmaterial nodeや
UV/normal境界での頂点分割を考慮し、FBXからの変換を無損失と仮定しない。
選択物だけのexportや材質の単純化で欠落を隠さず、対応表・形状・画像を照合する。
[glTF Validator](https://github.com/KhronosGroup/glTF-Validator)の形式・参照検査と、元シーンの完全性確認も区別する。
取込・作成・更新境界で内容を保証し、不変なasset本文を毎drawで再走査しない。

実配布`Bistro_v5_2.zip`は894,377,473 byteで、FBX独立解析は1,296 mesh Model・2,832,120三角形、
132材質・405画像を確認した。全meshにauthored normalとTextureUV/LightMapUVがあり、tangent/colorはない。
原本ZIP・検証済みglTF一式・環境HDRと比較PNGは[素材一覧](../../assets/bistro/README.md)のとおりGit LFSで共有し、
パス・byte数・SHA-256をmanifestへ固定する。作業用展開・Blender・host用中間データは`.tools/`に分ける。
標準のBlender処理が削除する2,894重複面には、逆向き980面、異なる法線やUVも含まれる。
検証profileでは頂点indexだけを分離し、全三角形・向き・材質・属性を保持して通常のMesh検査を通す。
FBXのraw CORNER normalを別属性に保存し、Blender内部の圧縮normalを経由せずglTFへ正規化出力する。
固定exporterの4桁丸めは7桁へ変更し、変更箇所・元/変更後functionのhashを記録する。
これらは既知FBX hashと固定Blender版に限定したoffline変換であり、一般入力やRendererの検証を無効化する方針ではない。

同梱READMEのPBR規約を使い、BaseColor RGB/alpha、ORMのR=AO/G=roughness/B=metal、発光RGBを明示接続する。
BC5 normalは正Zを復元してDX規約から緑成分を反転し、色とdataの扱いを分ける。
10発光材質のFBX色係数0.5を保持し、全ModelのCullingOffを両面材質へ対応させる。
alphaはopaque 120、mask 7、blend 5とした。maskのcutoff 0.5と分類は変換方針であり、元のartist intentの証明ではない。
ORM画像がない瓶ラベル1材質はnonmetal/roughness 0.5を明示した暫定値とし、元Phong値をPBR値と断定しない。

使用中の`MASTER_Glass_Dirty_MASKED`と`Paris_LiquorBottle_01_Labels`は画像alphaが全0で、計222面に影響する。
元の値を保持して`renderer-7ft.1.1.2`で解釈を追跡し、推測で不透明化した結果を忠実変換とは呼ばない。
PNGはbase mipだけの中間形式であり、元DDS・mip情報は保存する。実行用のmipとMikkTSpace生成はB2で確定する。
TANGENTは元にも検証glTFにもなく、[glTF仕様](https://github.com/KhronosGroup/glTF/blob/main/specification/2.0/Specification.adoc)に従いconsumerで生成する。
この構成の公式Validator警告1,591件と、保持した未使用LightMapUVに関するinfo 1,591件は原文を残す。
native API受入の警告/エラーゼロを緩和する例外ではない。

#### 描画方式の出発案

| 項目 | 第一案と採用時の確認 |
|---|---|
| Raster/PBR | 通常のForward、metallic-roughnessのGGX specular＋diffuse、normal map。太陽光＋IBLから始める構成の小ささを理由とし、Deferred等への性能優位は主張しない。材質棚卸しに必要な変換・emissive・alpha・両面を反映する。 |
| IBL | 静的環境を前処理し、diffuse irradianceとroughness別specularを使う。環境の出典/license/hash・回転・強度・前処理toolを固定し、環境内の太陽とdirectional sunの重複を調整する。 |
| 影 | 太陽のCSM＋PCF。分割数・解像度・距離・bias・filter・境界処理と安定化は、素材と巡回経路で画質・費用を確認して固定する。 |
| HDRとBloom | 線形Rec.709のRGBA16F候補へ描画し、利用側の多段Bloomを合成して固定露出・tone mappingへ渡す。Bloomの追加・無効化・交換でRenderer本体を編集しない。 |
| 出力 | SDR sRGBを初期表示とし、tone mapperはKhronos PBR Neutralを候補にする。linear入力・出力とsRGB転送を分け、encodeを一度だけ行う。formatのattachment/storage/sampled/filter等の実用途は両APIで照会・確認する。 |
| AA | 履歴・jitterを必須にしないFXAAを候補にし、AAなしの診断画像も保存する。細線・鏡面の時間方向のちらつきを解決済みとは扱わず、巡回で評価する。 |

PBR/IBLの算法は[Filamentの解説](https://google.github.io/filament/main/filament.html)、
影の算法は[MicrosoftのCSM解説](https://learn.microsoft.com/en-us/windows/win32/dxtecharts/cascaded-shadow-maps)、
tone mappingは[Khronosの参照shader](https://github.com/KhronosGroup/ToneMapping/blob/main/PBR_Neutral/pbrNeutral.glsl)を根拠にする。
CSM資料のD3D10/11向け実装説明をD3D12のAPI契約へ流用しない。
これは画質・構成の出発案であり、vendor固有の最適化や未計測の高速化ではない。
採用基盤・公開APIの選定は後続のB0課題で行い、Vulkan/VMA等の同期・寿命・能力契約と
[製品・開発の検証方針](../guides/validation-policy.md)を維持する。一般推奨からの新しい例外は設けない。

#### 再現・比較と完了の境界

asset/IBL/shader/設定のhash、座標・単位・projection、camera pose/FOV/near/far、巡回keyframeと固定時刻、
sun/IBL、露出、影・Bloom・AA、解像度を一組で保存する。全景・路地・材質近景・alpha・影境界を確認できる
視点を取得後に選び、自動露出や時刻変化・乱数jitterで比較条件を変えない。
最終SDRだけでなくBloom前HDRと必要な中間出力・差分を保存し、単純fixtureの厳密比較とBistroの許容誤差比較を分ける。
同API反復と二API差、既知の材質欠落・色変換ミス・影ずれを検出できることから、受入runより前に
絶対/相対誤差・誤差画素数/領域の閾値と根拠を固定する。輪郭等の一括除外で不具合を隠さない。

両APIの表示・自由操作・resize・最小化復帰・pending中の終了を実GPUで受け入れる。
最適化＋Renderer追加検証あり＋厳格native検証の経路と、Renderer追加検証・専用診断をコンパイル除外し
native検証を無効にした最適化通常経路で内容を別に照合し、
正常終了・native所有者破棄後までの警告/エラーゼロを既存方針どおり要求する。
初回読込/変換・upload・shader/pipeline準備と定常描画、CPU各phase・GPU・frame全体・メモリを分け、
warm-up・複数runのp50/p95・揺れ・VSyncと表示待ちを記録する。

準備の記録手順は[開発ガイド](../guides/development.md#bistro-preparation)に置く。
素材の版/hash・全材質/画像の棚卸しと、glTF/全参照fileのbyte一致による変換再現性は確認した。
全ゼロalphaの解釈、最終camera/path/light/IBL/露出等が残るため`renderer-7ft.1.1`は進行中とする。
画像閾値の校正と性能予算は各段階の実GPU結果で具体化し、最新の実行/未実行は
[検証状況](../verification/current.md)へ記録する。

### B0: Bistro基準・公開APIのUX・採用基盤の設計

`renderer-7ft.1`。次の具体的な課題を順に実施する。

| 課題 | 成果 |
|---|---|
| `renderer-7ft.1.1` | 素材・描画要求を棚卸しし、最終対象・品質・再現条件・比較方法を固定する。 |
| `renderer-7ft.1.2` | 第7章の代表利用コードを複数案で書き、UX、所有、資源アクセス、pipeline/binding/ABI、errorと拡張時の変更箇所を比較する。 |
| `renderer-7ft.1.3` | 既存継続/改修・自前置換・外部基盤利用を一次資料と小試作で比較し、両APIのWindows表示も含む総工数・品質・保守負担から採用案と移行範囲を決める。 |

**完了条件:** 利用コードと要求を根拠にAPI・採用基盤の第一案を説明できる。
外部基盤へ委譲する範囲とRendererが保証する範囲も明示する。
設計検証は広く行い、全将来機能の即実装や公開APIの凍結は要求しない。
候補を網羅する調査で長期化させず、共通の小さな描画・Compute例と比較観点を揃えて採用可能な第一案を選ぶ。
採用後にB1以降をbackend/契約/描画成果ごとの検証可能な縦切り課題へ分割する。

### B1: 公開パスと資源・shader契約の二API実証

`renderer-7ft.2`。B0の採用案で、独自shader/pipeline/binding、複数drawのRaster、
depth-only/color-only、Compute/copy、import/exportとGPU寿命を段階実装する。
資源format・範囲、shader信頼境界・ABI、宣言と実命令の整合、能力・errorを実証する。

**完了条件:** 効果専用操作をRenderer本体へ追加せず、同じ利用側パス定義と入力を二APIで実行できる。
標準描画も独自処理も同じGraph経路を使い、未投入・未完了・更新・破棄・失敗・回収を区別する。
以降の実シーンで分かった不備は設計へ戻して改訂する。固定操作の完全移植を先行条件にしない。

### B2: 利用側asset経路とテクスチャ付きBistro PBR

`renderer-7ft.3`。利用側で素材を変換・読込し、Rendererの資源・material・描画入力へ接続する。
必要な頂点属性、texture/sampler/mip、材質とalpha、GPU常駐と更新を縦切りにし、部分実証から最終対象へ広げる。

**完了条件:** geometry/material一覧と変換結果が対応し、対象Bistroをテクスチャ付きPBRで描ける。
ModelをRendererへ持ち込まず、取込/作成/更新境界の保証を保持し、毎drawでasset本文を再検査しない。
同じ入力と公開Graphを二APIへ接続する。

### B3: 影・線形HDR・トーンマッピング

`renderer-7ft.4`。B0で選ぶ照明・描画方式に従い、depth-only shadow、PBR照明、浮動小数点HDR中間画像、出力変換を組み合わせる。

**完了条件:** 固定camera/lightsの対象Bistroで影・材質・色空間とHDRから出力まで確認できる。
同じパス構成を二APIで実行し、機能ごとの内容照合と同期・寿命検証を実装時から行う。

### B4: 利用側の独自Bloomとパスの合成・交換

`renderer-7ft.5`。Renderer外のmodule/crateで独自shaderの多段Bloomを定義し、標準HDR出力へ接続する。
中間資源はモジュール内部へ隠し、出力handleで他の処理と組み合わせる。
B1の公開パスが動いた時点で小さなHDR入力を使う試作を始め、B2/B3と並行してUXを確認する。
Bistroへの最終接続はB3のHDR出力を使い、拡張性の問題発見を実シーン全体の完成待ちにしない。

**完了条件:** Renderer本体を編集せずBloomを追加・無効化・別実装へ交換できる。
同じ利用側コードを二APIで実行し、標準描画と同じ資源・同期・寿命・culling・診断を通る。
重複記述・所有負担・拡張時の変更箇所をレビューし、必要なAPI改訂を行う。

### B5: Bistro二APIの統合受入と組込性

`renderer-7ft.6`。各段階の検証を継続したうえで、1920×1080の固定view/camera pathとoffscreen比較、
両APIのWindows表示・自由移動、複数frame、更新・pending終了と補助sceneを統合確認する。
初回受入は既存RTX 3060環境とし、初回読込と定常描画を分けて測定する。

**完了条件:** 上表の描画・拡張性・二API・正しさ・所有・性能の証拠が揃う。
外部の利用コードから再現できるガイドを整え、実行/未実行と既知の制限を説明できる。
画像が出たことだけで拡張性や組込品質の達成を主張しない。

### 実測から選ぶ後続の最適化

`renderer-7ft.7`で物理aliasing、複数queue、並列記録、cacheやbarrier削減の必要性と採否を判断する。
旧M4のaliasing/複数queue実験を実装完了とは扱わず、Bistroの基準値を判断材料にする。
B0〜B5の一律blockerにはせず、採用時は能力・正しさ・利益と代償を確認して別課題へ分割する。
正しい比較経路を保持し、検証・profiling・最適化は独立に選択する。

## 16. 未確定事項と決定する時期

| 事項 | 決定時期・判断材料 |
|---|---|
| Bistroの版・対象・変換・camera/lights・品質 | B0の最初に棚卸しして固定する。部分実証と最終対象を区別する。 |
| APIの形、Graph意味論、資源範囲、所有・error | B0の代表利用コードで設計し、B1の二API実行とB2以降の実シーンから改訂する。 |
| 採用基盤、依存、内部RHI、crate配置 | B0で既存維持・改修・置換・外部基盤を比較する。現在の選択は維持義務ではない。 |
| 対象OS/GPU、最低能力、shader言語・compiler | 現行環境を材料にB0で再評価し、採用versionと再現条件を記録する。対象APIはVulkan/D3D12。 |
| 座標・色、texture/material、pipeline/binding/ABI | B0で要求を定め、B1で実証しB2/B3の素材・画質で確認する。 |
| Forward/Deferred、影、HDR format、alpha方式 | B0のBistro要求と実装費用から出発案を選び、B2/B3で検証する。 |
| 物理aliasing・複数queue等 | Bistro実測で必要性を判断する後続候補。先行実験や自前制御を一律必須にしない。 |
| 画質・性能予算とAPI採用範囲 | B0で比較方法を決め、各段階の実測とB5の統合受入で具体化する。API変更ゼロは保証しない。 |

未確定事項は局所的な暫定判断を妨げるものではない。可逆な判断は理由を記録して進める。一方、所有権・安全性・外部API互換性を暗黙の仮定のまま固定しない。

### 16.1 第二バックエンドの選定と準備条件

第二APIは**Windows上のD3D12**とする。M3の表示対象をWindowsから始める方針と、
利用可能なWindows実機上でVulkanと同じGPU・入力を比較できることを優先した本Rendererの判断である。
これはD3D12の性能優位や、全対象OSで二つのAPIを提供する方針を意味しない。
LinuxのVulkan経路とCPUのみのビルドを維持し、第二APIの公開境界にもネイティブ型を露出させない。

Metalも比較候補とした。Apple向けのAPI・shader検証手段がある一方、
現在利用できるMac実機・runnerは確認できていないため、最初の第二APIには選ばない。[^metal]
Apple展開が優先要件になった場合やWindows実機を維持できなくなった場合は、
利用可能な機材・shader変換・検証手段を揃えて選定を見直す。

以下は旧M4で確認した現行実装の範囲である。新計画の採用基盤と追加確認はB0/B1で決める。

| 項目 | 確認した範囲と制限 |
|---|---|
| 実機 | RTX 3060、driver 610.88、12 GiB。準備probeでDXGI hardware adapterと実Vulkan選択のvendor/device ID・LUID一致を確認した。 |
| native初期化 | 準備probeでhardware Device、SM6.0、RGBA8 typed UAV load/store、committed bufferの作成・map・破棄を確認した。製品runtimeとbuffer投入の範囲は後述する。 |
| ランタイム・SDK | `d3d12.dll`と`d3d12sdklayers.dll`は10.0.26100.9818。Windows SDK 10.0.26100を含む複数版と、Vulkan SDK 1.4.341.1のDXC・SPIRV-Cross・glslangValidator・spirv-valが存在する。 |
| 検証 | Device作成前のDebug Layer／GBV設定、InfoQueueとGBV有効の起動報告、最終COM Release後までの診断を3回反復確認。shaderのGBV実行と画像等価性は未実施。GBVは性能測定と分ける。[^d3dvalidation] |
| CI | Windows／Linuxのbuild・check・CPU試験を維持し、WindowsのD3D12単独／Vulkan併用を追加。実GPU runnerはなく、管理者のWindows実機で逐次受入する。[実行手順](../guides/development.md#m4のd3d12準備と実gpu受入経路)を使う。 |

旧M4は固定操作をD3D12へ移植してから公開契約を抽出する計画だった。未着手分はB0〜B5へ置換し、次の作業をB0の要求・利用例・方式選定とする。二APIで同じパスと入力を比較する目標は維持する。
D3D12での表示経路を、共通契約の検証に先立つ必須条件にはしない。
ただし[Bistroの確定要件](#bistro-requirements)により、両APIでのWindows表示は最終受入の必須条件である。

shaderは既存GLSL／SPIR-VからSPIRV-CrossでHLSLを生成し、DXCでDXILへ変換する経路を採用した。[^spirvcross][^dxc]
固定5種類の再生成一致とreflectionを確認し、tool版・hash・引数・ABIを[ツールガイド](../guides/tooling.md#d3d12の固定shader生成)と管理下metadataに記す。
binding、combined sampler、行列・座標・データ配置の等価性は生成成功だけで判断せず、実出力と照合する。
HLSLを単一ソースへ移す案は代案とし、既存Vulkan基準の再生成一致と変更量を比較してから判断する。
Rust bindingsはMicrosoftの`windows-bindgen`によるbackend-private生成を採用する。
D3D12・DXGIと必要なWin32 APIだけを生成し、生成型はD3D12バックエンド内部に閉じる。
準備範囲ではDXCのnative APIを使わず、固定CLIでshaderを事前生成する。
再利用ライブラリの依存量と版の衝突を抑える公式推奨に従う選択である。[^windowsrust]
M4準備でgenerator／metadata／runtimeを0.100.0へ固定し、MSRV 1.95が本リポジトリのRust 1.99内であることを確認した。
生成toolは独立workspaceとlockfileに分離し、限定filterとLF固定の生成済み単一RustファイルをGit管理する。
通常のRustビルドへgenerator・SDKを要求せず、再生成一致と生成toolを含む依存監査を確認した。
固定入力と手順は[ツールガイド](../guides/tooling.md#d3d12-bindingsの生成)を参照する。

代案の`windows 0.62.2`は既存allocatorとの接続が容易だが、0.62系への長期保守・修正backportの保証は確認できていない。
この短期互換性だけを理由に旧系列へ固定せず、allocatorは採用bindingsとの型・ABI・所有権・保守状況を別途検証して選ぶ。[^windowsallocator]
COM参照の保持とGPUの利用期間を区別し、resource・heap allocation・descriptor領域・command allocator等の解放や再利用をFence完了で保護する。[^d3dlifetime]

現行D3D12のbuffer基準には`CreateCommittedResource`を使う。
resourceと暗黙のheapを一つの所有者で保持でき、private生成型と既存allocatorの公開型を
未検証のcastで接続する必要がない。DEFAULT bufferは`COMMON`、UPLOADは`GENERIC_READ`、
READBACKは`COPY_DEST`で作成する。[^committed]
採用D3D12ランタイムはDEFAULT bufferの初期`COPY_DEST`を無視して`COMMON`とし、警告1328を報告するため、
準備probeと製品runtimeは実状態と一致する`COMMON`を使う。Graph実行はこの状態から依存を計画する。
代案の外部suballocatorは型・COM所有権・配置条件の接続確認を要するため、この基準には導入しない。
resourceごとのheapによるメモリ・作成費用を許容する単純な基準であり、性能改善とは扱わない。
上限付きの資源再利用と物理aliasingは現行D3D12では未実装である。
旧M4-06/16の順序を新計画へ持ち越さず、前者はB1の採用基盤で実証し、後者はBistro実測から採否を判断する。
準備probeは作成と破棄だけの確認であり、buffer投入後の寿命とreadback内容は製品runtimeの受入で扱う。

準備は`renderer-we8.1`・`.2`、`renderer-6qc`（adapter・検証・shader経路）、
`renderer-k51`（ローカル実GPU反復実行基盤）で実施した。
現在の`d3d12` featureは、次のDeviceとbuffer実行までを提供する。

<a id="d3d12-runtime"></a>

### D3D12のDeviceとbuffer実行

Windows限定の`D3D12Runtime`が、hardware adapter、Device、単一DIRECT queueと明示終了を所有する。
adapterのvendor／device IDとLUID、照会した能力をnative型なしで公開する。
CPUだけのGraph構築とLinuxのVulkan経路には、D3D12のnative依存を追加しない。

D3D12 Deviceはadapter LUIDごとのsingletonであり、作成済みDeviceへのDebug Layer／GBV再設定は
Device removalを起こし得る。[^d3ddevice][^d3ddebug]
このruntimeはprocess内の最初の診断設定を固定し、同じ設定の生存runtimeではDevice所有者を共有する。
設定を切り替える要求はnative設定前に拒否する。複数runtimeのqueue操作も共有所有者で直列化する。
外部のD3D12 Deviceが既に作成されたprocessで後から診断を有効にする使い方は対象外とし、
診断を使う場合はアプリ内のD3D12 Device作成より先に初期化する。

`development-validation`は`d3d12-diagnostics`も有効にし、native検証の既定を
`D3D12ValidationMode::Required`にする。追加検証なしの既定は`Disabled`である。
独立した`d3d12-diagnostics`なしの明示`Required`は対応外として拒否する。
診断なし構成ではDebug Layer／GBV設定、InfoQueue、callback、診断専用の履歴をコンパイル除外する。
nativeエラーと元の原因、実完了、Device removal、資源保持は製品構成にも残す。
monitorは診断状態だけを保持し、Deviceやqueueの寿命を延ばさない。
runtimeを終了しても、別runtimeや未完了処理がnative所有者を保持する間は終了後診断の確認済みとは扱わない。
最終native解放まで観測し、診断の有効要求と取得できた実効状態を区別する。
終了確認の対象はRendererが所有するnative参照の解放までであり、外部ライブラリが保持する
同じsingleton Deviceの最終破棄まで証明するものではない。

`D3D12Runtime::prepare`は共通の不変`CompiledGraph`から、liveなbuffer upload・copy・readbackを
native資源と命令へ具体化する。Graphのproducer・範囲・culling解析を複製しない。
Graph構築で保証した内容を再走査せず、未対応操作、D3D12固有の上限と資源作成条件を準備境界で扱う。
現在はtransient bufferだけを受け入れ、export、liveなimport・画像・その他の操作は明示エラーとする。
不要パス除去後の未使用操作を、そのbackendが未対応であることだけを理由に失敗させない。
bufferの要求sizeはライブラリの上限2 GiB以内とし、実際のnative割当失敗やVRAM budgetとは区別する。

DEFAULT、UPLOAD、READBACKのcommitted resourceを使い、legacy barrierで全bufferの状態を追跡する。
DEFAULT bufferは一つのcommand list内で明示的に遷移し、implicit promotion／decayによるbarrier削減は
現段階では採用しない。状態と命令の対応を確認しやすい基準を優先する選択であり、余分なbarrier費用は残る。[^d3dbarriers]
同じbufferの非重複copyは、別のscratch bufferを介した二段copyで共通Graphの意味を保つ。
一つのbufferへ`COPY_SOURCE`と`COPY_DEST`を同時に要求しない。
Enhanced Barriers、外部suballocator、資源pool、画像、import／exportをこの範囲へ先行導入しない。

`submit`または`submit_prepared`は待機せずに投入し、`D3D12Submission`がcommand allocator／list、
全buffer・allocationとDevice所有者を実GPU完了まで保持する。
未投入のprepared処理の破棄はキャンセルであり、通常に解放できる。
`poll`／`wait(timeout)`で対象Fenceの正常完了を確認してから、`D3D12SubmissionResult::readback`で
CPUへコピーする。timeoutは結果を返さず、同じsubmissionを再度待てる。readbackは一度だけ消費する。
Fenceの`UINT64_MAX`やDevice removalを正常完了とみなさない。[^d3dfence]

`ExecuteCommandLists`後の`Signal`失敗は未投入rollbackを保証できない。[^d3dexecute][^d3dsignal]
その場合はDeviceを停止し、GPU利用終了を証明できない所有連鎖を保持する。
未完了submissionのDropは対象Fenceだけを最大5秒の予算で待ち、完了を証明できなければ同じ保持を行う。
この異常経路では保持資源を回収できない。通常経路は明示的な完了確認で回収し、Drop待機へ依存しない。
通常のsubmitへQueue／Device全体の暗黙waitを追加しない。

共通Graphの全byte一致、終了後までの厳格診断、最適化された診断なし経路の内容を別runで確認する。
実行方法は[開発手順](../guides/development.md#d3d12-buffer-acceptance)、
実行済み・未実行の範囲は[現在の検証状況](../verification/current.md)に記す。
画像・上限付き再利用・canonical import/exportは現行D3D12で未実装である。旧M4-05〜07の順序を必須にせず、B0の選定を受けてB1以降へ組み直す。
一般RHIや将来の描画・binding契約をこのbuffer実行だけで確定したとは扱わない。

## 17. Codexの作業手順

### 17.1 着手時

最初にリポジトリの指示・状態・実装を読み、依頼対象の段階を特定する。本書の章構成を、そのまま大量のクレートやファイルへ機械的に変換しない。

既存の変更を尊重し、関係のないリファクタリングや削除を同時に行わない。現在のコードと本書が衝突する場合は、差分と移行方法を記録する。既存基盤の再利用を必須にせず、B0の比較から維持・改修・置換を選ぶ。置換する場合も、失われる対応・保証、移行方法と検証を明記し、未検証の新経路を完成扱いにしない。

一つの作業では、検証できる縦切りの機能または限定的な設計変更を選び、変更対象・受入条件・非対象を決める。必要な仕様は、採用バージョンの公式ドキュメントと実際の依存コードで確認する。存在しないAPIを推測して使用しない。

Vulkan関連の実装・レビューでは、[AGENTS.mdの実装規約](../../AGENTS.md#vulkan-implementation-and-review)に従い、
[ベストプラクティス](../guides/vulkan-best-practices.md)の該当節を適用する。
Vulkan/VMAの契約と現行Renderer方針は必須、一般推奨は原則採用とし、異なる選択には理由・代案・根拠を残す。
機材依存の最適化は正しさと計測した効果・代償で判断する。後続機能の解説を理由に今回の実装範囲を広げない。

### 17.2 実装と検証

典型的な確認コマンドは以下である。これは本書作成時に実行した結果ではなく、リポジトリに合わせて設定する基準例である。

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

実際にはtarget・backend feature・GPUテストを分けたCI matrixを用意する。OS専用機能や排他的featureがあるため、常に`--all-features`で検証できるとは仮定しない。既存のlint違反を修正する場合も、依頼と無関係な大量変更に広げない。

CPUテスト、実GPUテスト、性能測定を分けて実行・報告する。GPUへアクセスできない環境では、実GPU確認を「未実行」として理由を残す。ソフトウェア実装やモックの成功を、対象GPUでの性能・同期・ドライバー互換性の検証と同一視しない。

テストを通すために検証レイヤーを無効にしたり、全フレーム待機を隠れて追加したり、画像差分の許容値を理由なく広げたりしない。暫定策が必要なら、影響・除去条件・追跡先を残す。

### 17.3 変更完了時の報告

報告には、実装した内容、変更した契約、実行した検証、未実行の検証、既知の制約、計測結果、次の最小タスクを含める。実行したコマンドと環境を記録し、実測値と推定値を混ぜない。

重大な設計判断は現在の設計文書を更新する。背景、採用案、比較した代替案、影響、見直す条件を短く記載し、検証結果は現在の検証状況へまとめる。段階別・日付順の記録を増やさず、置き換え前の内容はGit履歴に残す。文書だけを更新して、対応する動作確認まで完了したように報告しない。

## 18. 変更ごとのDefinition of Done

以下を満たした範囲だけを完了とする。機能に無関係な項目は、その理由を明示して対象外にできる。

- [ ] 対象タスクの受入条件を満たし、非対象の機能を不用意に追加していない。
- [ ] 公開境界にVulkan・VMAの型や暗黙の所有権が漏れていない。
- [ ] Vulkan関連変更はベストプラクティスの該当要件に従い、一般推奨からの変更理由・根拠と、実行済み／未実行の検証を記録した。
- [ ] 関連する同期・寿命・失敗経路のテストがあり、既存テストを維持している。
- [ ] `unsafe`の前提と責任範囲を説明できる。
- [ ] 実GPUの検証状況を、成功・失敗・未実行に分けて記録した。
- [ ] 最適化を含む場合は基準経路との比較結果があり、画質・性能・メモリの変化を説明できる。
- [ ] 診断情報から、変更した処理とパス・リソースを対応づけられる。
- [ ] API・操作手順・設計判断・既知の制限を、変更に応じて更新した。

## 19. 最初にCodexへ渡す作業指示

次の指示を入口とする。配置先は既存リポジトリの文書構成に合わせる。

```text
この実装方針を読み、まず既存リポジトリの状態を確認してください。
目標はBistroのテクスチャ付きPBR・影・HDR/トーンマッピングと、
利用側独自Bloomを、同じパス定義でVulkan/D3D12へ接続することです。

B0で素材・画質・API利用例を具体化し、既存維持・改修・置換・外部基盤を比較してください。
既存基盤の維持や自前実装を目的にせず、進めやすさ・総工数・UX・品質で選んでください。
公開APIを少数の成功例だけで決めず、実装は検証可能な縦切りに分けてください。

Beadsの現行Bistro計画から依頼範囲の未完了タスクを選び、
そのタスクの実装と実行可能な検証まで進めてください。
GPU検証ができない場合は、実行済みの確認と未検証項目を明確に分けてください。
所有権・同期・公開APIに影響する判断は、理由を現在の設計文書へ反映してください。
```

## 20. 参考資料

以下は外部ライブラリ・APIの仕様確認に用いた一次資料である。本文の設計・実装順序は本プロジェクトの方針であり、参考実装の採用方法や開発順序をそのままコピーしたものではない。確認日は2026-11-12。変更される資料については、実装時にも採用バージョンを確認する。

[^ash]: ash公式README。Vulkanに近い薄いラッパー、生成される定義、検証・安全性の責任に関する説明。`https://github.com/ash-rs/ash`

[^ashdocs]: ash 0.38.0+1.3.281 APIドキュメント。`https://docs.rs/ash/0.38.0+1.3.281/ash/`

[^vkmem]: vk-mem 0.5.0。VMAのRustバインディング、Allocationとサブアロケーションの概要。概要内の古いサンプルをそのまま採用せず、実際のAPI定義と照合する。`https://docs.rs/crate/vk-mem/0.5.0`

[^vkmemmanifest]: vk-mem 0.5.0のCargo.toml.orig。ashの互換バージョンとfeatureの確認。`https://docs.rs/crate/vk-mem/0.5.0/source/Cargo.toml.orig`

[^vmbuild]: vk-mem 0.5.0のbuild.rs。C++のVMA実装をビルドする処理。`https://docs.rs/crate/vk-mem/0.5.0/source/build.rs`

[^allocator]: vk-mem 0.5.0 Allocator API。作成時の寿命条件、bind / map / flush / invalidate、破棄・解放の契約。`https://docs.rs/vk-mem/0.5.0/vk_mem/struct.Allocator.html`

[^vmaalias]: VMA公式資料「Resource aliasing (overlap)」。共有メモリの要件と利用上の注意。オンライン資料とvk-mem同梱VMAのバージョン差にも注意する。`https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/resource_aliasing.html`

[^features]: Khronos Vulkan Guide「Enabling Features」。問い合わせと有効化の手順。`https://docs.vulkan.org/guide/latest/enabling_features.html`

[^sync]: Khronos Vulkan Guide「Synchronization Examples」。アクセスに対応する依存、バリア、レイアウト遷移の例。`https://docs.vulkan.org/guide/latest/synchronization_examples.html`

[^queues]: Khronos Vulkan Guide「Queues」。キュー間順序、host側同期、ハードウェアとの対応。`https://docs.vulkan.org/guide/latest/queues.html`

[^wsi]: Khronos Vulkan Guide「Swapchain Semaphore Reuse」。present待機Semaphoreの再利用。`https://docs.vulkan.org/guide/latest/swapchain_semaphore_reuse.html`

[^wsirecreate]: Khronos Vulkan Samples「Swapchain Recreation」。旧Swapchainとpresent関連リソースの寿命管理。`https://docs.vulkan.org/samples/latest/samples/api/swapchain_recreation/README.html`

[^validation]: Khronos Vulkan Guide「Vulkan Validation Overview」。Valid Usageと検証レイヤー。`https://docs.vulkan.org/guide/latest/validation_overview.html`

[^debug]: Khronos Vulkan Guide「VK_EXT_debug_utils」。オブジェクト名、ラベル、debug messenger。`https://docs.vulkan.org/guide/latest/extensions/VK_EXT_debug_utils.html`

[^atom]: O3DE Atom「AZ::RHI::FrameGraphCompiler」。共通コンパイルとプラットフォーム固有処理の分担。`https://docs.o3de.org/docs/api/gems/atom/class_a_z_1_1_r_h_i_1_1_frame_graph_compiler`

[^d3dvalidation]: Microsoft「GPU-based validation and the Direct3D 12 Debug Layer」。Device作成前のGBV設定と、計装・非同期出力・性能への影響。`https://learn.microsoft.com/en-us/windows/win32/direct3d12/using-d3d12-debug-layer-gpu-based-validation`

[^dxc]: Microsoft DirectX Shader Compiler公式README。HLSLからDXIL／SPIR-Vへの生成。`https://github.com/microsoft/DirectXShaderCompiler`

[^spirvcross]: Khronos SPIRV-Cross公式README。SPIR-VからHLSL等への変換と制約。`https://github.com/KhronosGroup/SPIRV-Cross`

[^windowsrust]: Microsoft windows-rs公式README。再利用ライブラリのfocused crate・private binding生成の選択指針。実装時には採用版と照合する。`https://github.com/microsoft/windows-rs`

[^windowsallocator]: gpu-allocator 0.28.0の公開manifestは`windows >=0.53, <=0.62`を要求する。新しい生成型との接続を未検証のまま互換と扱わない。`https://docs.rs/crate/gpu-allocator/0.28.0/source/Cargo.toml`。Microsoft担当者の旧系列の不具合への回答はmasterでの修正と0.62での回避策を示し、0.62へのbackportを約束していない。`https://github.com/microsoft/windows-rs/issues/5189#issuecomment-6448298133`

[^d3dlifetime]: Microsoft「Important Changes from Direct3D 11 to Direct3D 12」。明示同期と、アプリが管理する資源・descriptorメモリの寿命。`https://learn.microsoft.com/en-us/windows/win32/direct3d12/important-changes-from-directx-11-to-directx-12`

[^committed]: Microsoft「ID3D12Device::CreateCommittedResource」。resourceと暗黙のheapの所有、UPLOAD/READBACKの初期状態、最後の参照を解放する前のGPU利用終了条件。`https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12device-createcommittedresource`

[^d3ddevice]: Microsoft「D3D12CreateDevice」。adapter LUIDごとのDevice singletonと、removed Deviceの再作成失敗。`https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-d3d12createdevice`

[^d3ddebug]: Microsoft「ID3D12Debug::EnableDebugLayer」。Device作成後の有効化はDeviceをremoveする。`https://learn.microsoft.com/en-us/windows/win32/api/d3d12sdklayers/nf-d3d12sdklayers-id3d12debug-enabledebuglayer`

[^d3dfence]: Microsoft「ID3D12Fence::GetCompletedValue」。Device removal時の戻り値は`UINT64_MAX`。`https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12fence-getcompletedvalue`

[^d3dexecute]: Microsoft「ID3D12CommandQueue::ExecuteCommandLists」。投入呼び出し自体は戻り値を持たない。`https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12commandqueue-executecommandlists`

[^d3dsignal]: Microsoft「ID3D12CommandQueue::Signal」。queue上のGPU側signalをHRESULTで報告する。`https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12commandqueue-signal`

[^d3dbarriers]: Microsoft「Using Resource Barriers to Synchronize Resource States in Direct3D 12」。read／write stateの制限、bufferのpromotion／decayと明示遷移、barrier削減の推奨。`https://learn.microsoft.com/en-us/windows/win32/direct3d12/using-resource-barriers-to-synchronize-resource-states-in-direct3d-12`

[^metal]: Apple「Metal」。AppleプラットフォームとAPI／shader検証・診断ツールの概要。`https://developer.apple.com/metal/`
