# RenderGraph設計のベストプラクティス

調査日: **2026-11-12**。RenderGraph／FrameGraphの意味論、コンパイラー、GPU実行、
資源管理、エンジンとの境界を対象に、公開されたエンジン資料・実装とAPI仕様を比較する。
唯一の標準APIを定める文書ではなく、設計・レビュー時の判断材料を整理したガイドである。
網羅対象は一般的なリアルタイム描画・Compute・転送・表示であり、全論文・全エンジンの調査完了を主張しない。

**現行実装の契約は[RenderGraphの設計](../design/rendergraph.md)を正とする。**
本書に登場する機能が実装済みという意味ではない。導入順序は[実装方針](../design/implementation-policy.md)、
Vulkan固有の要件は[Vulkanベストプラクティス](vulkan-best-practices.md)、
実行結果は[現在の検証状況](../verification/current.md)を参照する。

## 目次

- [1. 根拠と適用範囲](#evidence)
- [2. 公開アーキテクチャの比較](#architectures)
- [3. 意味論と責務の分離](#semantics)
- [4. パス宣言と実行API](#passes)
- [5. リソースの同一性・version・範囲](#resources)
- [6. 内容の有効性とAttachment](#contents)
- [7. 依存解析・不要パス除去・コンパイル](#compilation)
- [8. 同期とバックエンドへの具体化](#synchronization)
- [9. import/export・履歴・Presentation](#external)
- [10. GPU寿命・失敗・再利用](#lifetime)
- [11. メモリ配置とエイリアシング](#aliasing)
- [12. スケジューリングと並列化](#scheduling)
- [13. パス統合・tile memory](#merging)
- [14. キャッシュとCPU負担](#cpu)
- [15. 移植性と拡張機能](#portability)
- [16. 診断・検証・性能測定](#verification)
- [17. 設計選択とレビュー基準](#decisions)
- [18. 本Rendererへの適用](#application)
- [一次資料と更新方法](#sources)

<a id="evidence"></a>

## 1. 根拠と適用範囲

| 区分 | 本書での意味 |
|---|---|
| **API契約** | 対象APIの同期・寿命・使用条件。最適化の都合で省略できない |
| **一般推奨** | 複数の公開設計から採用理由を説明できる出発点。特定のクラス構成を要求しない |
| **設計上の選択** | 正しい方式が複数ある。要件・利用API・複雑さ・計測から決める |
| **条件付き最適化** | 対応能力と正しさを確認し、対象GPU・負荷で利益と副作用を測って採用する |
| **本書の整理・推論** | 資料を比較して導いた設計案や試験案。引用元の仕様・製品実装そのものではない |

節内の「推奨」「避ける」は、API契約と明記した箇所を除き、本書の整理・推論である。
資料の直接要約は「公開例」として区別する。個別製品の制限をRenderGraph一般の必須条件へ移さない。
資料と推論が食い違う場合、APIの可否は使用版の仕様、現在のRenderer方針は既存の設計文書で判断する。

公開実装は固定revision、製品ドキュメントは対象版を記録する。`latest`や版指定のないページは動的資料である。
特にVulkanの本リポジトリの基準は1.3、SDKは1.4.341.1であり、資料の新機能を自動的に利用可能とはしない。
この調査では設計資料の読解を行い、比較対象のエンジンのビルド・実行やGPU性能の再現は行っていない。

<a id="architectures"></a>

## 2. 公開アーキテクチャの比較

| 公開例 | 参考になる考え方 | そのまま一般化しないもの |
|---|---|---|
| Frostbite FrameGraph、GDC 2017 | Setup→Compile→Execute、アクセス宣言、writeのversion、不要処理の除去、寿命解析 | 当時のコンソール・ESRAM事情、測定値、blackboard等のAPIの形 |
| Unreal RDG | shader parameterと依存の連携、外部資源、検証・可視化、最適化を無効にする診断経路 | UnrealのRHI・マクロ・スレッドモデル・builderの制約 |
| Unity URP RenderGraph、6000.0 | パス種別とアクセス宣言、import資源、Unsafeパスに伴う制約 | Unityのハンドル型、URPのパス統合規則、import書き込みのculling方針 |
| Filament FrameGraph | リソースversion・依存グラフ・compile・実行を具体的な小さめの実装で追える | Filamentのbackendと描画機能に合わせた型・資源粒度 |
| O3DE Atom FrameGraph | Scope、attachmentの利用、共通compileとAPI固有の計画の分離 | AtomのScope／Attachment DBやJob構成 |
| AMD Render Pipeline Shaders（RPS） | 宣言からruntimeの計画・記録を作る別の実装例、スケジュールとメモリの観測 | 専用言語・SDKの導入、backendの対応範囲、最適化の効果 |
| NVIDIA Falcor | 永続Graphと変更時の再compile、pass reflection、出力起点の実行計画 | 研究用frameworkのAPI、field名のaliasと物理heap aliasingの違い |

出典: [Frostbite講演スライド][frostbite]、[Unreal RDG][unreal]、[UnityのRenderGraph概要][unity-intro]、
[FilamentのFrameGraph][filament]、[Atom FrameGraph][atom]、[RPS][rps]、[Falcor][falcor]。
これらは代表例であって、いずれか一つの方式を標準とする根拠ではない。

ソースで確認した違いも残す。Filamentの確認版は生存パスの宣言順を保ち、
あらゆる順序へ並べ替えるschedulerの例ではない。Atomはasync区間で一時資源の寿命を保守的に延ばす。
Falcorはdirty時に再compileする。RPSの公開headerには、まだ予約扱いのschedule flagもあり、
名前の存在を実装済み機能の証拠にしない。[Filamentソース][filament-source]、
[Atom compiler][atom-compiler]、[Falcorソース][falcor]、[RPS header][rps-header]

<a id="semantics"></a>

## 3. 意味論と責務の分離

RenderGraphは「パスをトポロジカルソートするもの」より広い。
少なくとも、次の情報は混同しない。

| 情報 | 答える問い |
|---|---|
| 論理dataflow | このreadは、どのwriteの結果を読むか |
| 物理アクセスのhazard | 同じ資源・メモリを使う操作の間に、どの順序が必要か |
| 内容の有効性 | 読む範囲に定義された値があるか |
| native状態 | stage／access／layout／queue ownership等の条件は何か |
| 実行の完了 | CPU利用・破棄・再貸出しが可能になったか |

責務は次のように分けると検証しやすい。これは論理的な分離であり、同数のcrateやtraitを作る提案ではない。

```text
パス・アクセス・出力の宣言
  → 意味論の検証と必要な処理の決定
  → backend能力を使った実行順・資源配置・同期の計画
  → native命令の記録・submit
  → 完了追跡・結果取得・資源回収
```

公開例: FrostbiteはSetup／Compile／Executeを分離する。[講演スライド][frostbite]
Atomは共通コンパイルとプラットフォーム固有処理を分ける。[FrameGraph][atom]
本書の推奨は、この境界でGPUなしの解析テストと実GPUテストを分離することである。

<a id="passes"></a>

## 4. パス宣言と実行API

### アクセス宣言を実際の操作へ結び付ける

パスには名前、種類（Raster／Compute／Copy等）、入力・出力、アクセス範囲、用途、
attachment操作、外部副作用を宣言させる。`read`／`write`だけでは、samplingとstorage、
vertex入力とindirect入力等に必要なnative同期を決められない。

宣言の二重管理を減らすには、型付きbinding、attachment builder、操作builder、shader reflection等を使う。
ただしreflectionだけではデータ依存のdescriptor indexやGPU addressの参照先集合を確定できない。
「宣言した資源を実行contextだけから取り出せる」構造にすると誤用を減らせるが、
任意shaderやnativeハンドルを許した場合の完全な安全性証明にはならない。
GPU addressはAPIの有効化条件も別に必要である。[Vulkan Buffer Device Address][bda]

公開例: Unrealはpass parameterに依存情報を持たせる。[RDG][unreal]
Unityは通常の宣言型パスとUnsafeパスのAPIを使い分ける。[パス作成][unity-pass]、[Unsafe pass][unity-unsafe]
これをRustへ適用するなら、builderと記録contextの責任を分け、contextがGraphの宣言外へ
自由にアクセスできる経路を局所化する、という設計案になる。

### 実行callbackとパス粒度

実行callbackは遅延・並列実行・cullingを前提とし、借用した短命データを後で参照しない。
必要な入力は所有・共有・arena保持のいずれかで実行時まで有効にする。
CPUのゲーム更新、乱数列の進行、外部イベント通知を、除去され得るcallbackへ混ぜない。
GPUで観測可能な副作用を持つパスは、その副作用を宣言して保持する。
公開例: Unrealも実行lambdaの副作用を避けるよう説明する。[RDG][unreal]

一つのdrawを必ず一つのGraphノードにする必要はない。依存や資源用途が変わる境界で分割し、
パス内のdraw list・batchはまとめられる。パス内で複数dispatchを行う場合は、その間の同期の
所有者を決める。Graphがパス境界しか解析しない設計では、パス内部の依存を自動解決できない。
Copy／Raster／Computeの境界と、native render pass／command buffer／submitの境界も別概念である。

同じパスでのread/write宣言も、attachmentを同時にsampleするfeedback loop等のnative合法性を
自動的に満たさない。用途の組み合わせを検査し、対応feature・同期を提供できなければ拒否する。
機能moduleやblackboardはGraphを組み立てる配線として使え、一つのmoduleが複数passを追加してもよい。
共有状態を経由してアクセス宣言を隠さないことが重要である。

<a id="resources"></a>

## 5. リソースの同一性・version・範囲

### 識別子を分ける

論理ハンドル、内容version、物理resource、View、allocation、runtimeの世代を区別する。
異なるViewが同じ画像の重複範囲を指す場合、別の独立資源としてhazardを隠さない。
importも物理資源のcanonicalな同一性へ結び付け、別ハンドルの二重importで状態を分裂させない。

Graph内のハンドルが別Graphや削除後の資源へ誤って通用しないよう、owner・世代等を検査する。
Rustの型はbuffer/image等の種別を区別する助けになるが、GPU完了やnative資源の所有を自動で証明しない。

### 明示versionと可変資源

| 方式 | 利点 | 必要な注意 |
|---|---|---|
| writeが新しい論理versionを返す方式 | producerが明確で、上書き前後の値を区別しやすい | 部分書き込み・read_modify_write・同じ物理資源への割当を定義する。versionの数だけallocationは不要 |
| 可変ハンドルと宣言順でproducerを追う方式 | 操作を素直に記述でき、既存の可変資源APIに接続しやすい | 各readが読む値、forward依存の意味、部分範囲のproducerを明文化する |

公開例: FrostbiteとFilamentにはwrite versionを使う設計がある。[Frostbite][frostbite]、[Filament][filament]
そこから全GraphへSSA形式を必須とは結論しない。
重要なのは、並べ替えた結果が「別のwriteの値を読む」ことを許さない意味論である。
新versionを作っても、旧内容の保存や旧handleでの再アクセスまで保証されるとは限らない。
例えばFilamentの確認版はslotのversionを更新して旧handleを無効にする。
どのversionをいつまで参照できるかもAPIの契約に含める。[Filamentソース][filament-source]

### 粒度は正しさと解析費用の両方から選ぶ

bufferはbyte範囲、画像は少なくとも対応するaspect／mip／layerを表現する。
未対応の粒度は明示的に拒否するか、全体アクセスへ安全側に拡大する。
部分書き込みを全体初期化へ拡大してはいけない。
画像の矩形有効範囲、shaderの動的アクセス範囲まで精密に推論する方式は費用が大きい。
最初は全subresourceの書き込みを要求するなど、利用者が守れる保守的な契約も選択肢になる。
論理byte範囲が非重複でも、native APIの同時アクセス・cache粒度の規則は別途満たす。
D3D12ではbuffer等の非重複領域への書き込みでも同期が必要になる条件があり、
論理解析だけで全backendの並列実行を保証しない。[Enhanced Barriers仕様][d3d-enhanced]

<a id="contents"></a>

## 6. 内容の有効性とAttachment

新規allocationやalias切替後の内容は、初期化済みと扱わない。
画像layout、メモリが以前使われた事実、read_write宣言は、必要な値の存在を証明しない。
未初期化read、部分範囲の穴、discard後のreadを、明確な契約で拒否する。

| 操作 | 内容の意味 |
|---|---|
| Load | 対象render areaの既存内容が必要 |
| Clear | 対象render areaに値を確立する |
| load Discard | 旧内容を要求しない。描画が全画素を書く保証とは別 |
| Store | 有効な結果を後続用途へ残す |
| store Discard | 対象範囲を後続の有効な入力としない |
| blend／depth test／storage read-modify-write | 書き込みだけに見えても旧値を読む可能性がある |

**API契約:** Vulkanのload/storeはrender area等の定義範囲に作用する。
`UNDEFINED` layoutはゼロ初期化ではなく、旧内容を保存しない選択である。
`DONT_CARE`も無アクセスを意味しない。
[VkAttachmentLoadOp][load-op]、[VkAttachmentStoreOp][store-op]、[VkImageLayout][image-layout]

Clear／Storeの自動推論は、全consumer、外部へのexport、パス統合、resolveを考慮して行う。
解析で証明できない全面上書きを、パス名やfullscreen drawの形だけで推定しない。
scissor、discard、depth/stencil、sample mask、条件付きshader書き込みが穴を残すためである。
MSAAのresolve元・先、depthとstencilのaspectも個別に扱う。
任意shaderの実際の全面書き込みは一般に自動証明できないため、明示的なfull-write契約、
制約付き操作、保守的なclear等のどれで内容を保証するかを定める。

<a id="compilation"></a>

## 7. 依存解析・不要パス除去・コンパイル

### dataflowとhazardを別々に扱う

例として、同じ資源へAが全面書き込み、Bが全面上書きし、CがBの値だけを読むとする。
外部副作用がなければAは不要になり得る。A→BのWAW制約だけを生存根拠にするとAが残る。
逆に必要なreaderを後のwriterより先に置くWAR制約は、値を生成しなくても実行時に必要である。
これは本書の依存解析上の整理であり、API仕様がcullingアルゴリズムを指定しているわけではない。

推奨する解析の流れは次のとおり。特定の実装順を必須とはしない。

1. 宣言・範囲・同一性と、各readが読むproducerを検査する。
2. export／readback／観測可能な外部書き込み／明示副作用をrootに、生存するdataflowを求める。
3. 生存パスの実際のアクセスから必要なRAW／WAR／WAW・明示順序を組み立てる。
4. 循環を検査し、再現可能な順序でscheduleする。
5. backend能力を使い、同期・資源配置・記録単位へ具体化する。

除去対象のwriterが途中にいた場合も、生存パス間のhazardを失わないことが必要である。
deadなパスの不正入力や循環をエラーにするかはAPI方針として決める。
検査をcullingの前に行う設計は、機能のON/OFFで潜在的な誤りが隠れるのを防げる。

### 解析量と決定性

トポロジカルソート自体はO(V+E)でも、Eを全パス対から作ると二次増加し得る。
資源・範囲ごとのwriterとreader frontierを追い、不要な推移的辺の実体化を避ける。
ただし重複範囲が多い入力では必要な辺や区間数自体が増える。全入力に線形と約束しない。
集合の反復順、tie-break、同名パスの識別を定義し、同じ宣言から同じ計画を再現可能にする。

compile失敗時にはpass、resource、範囲、期待したproducer、依存経路を返す。
CPU payloadやnative資源を丸ごと複製することと、意味論の解析を結合しない。
削除したパスは診断用の名前・理由だけを残す設計も可能である。

### 計画の変更は後段解析へ反映する

resolve／copyの自動挿入、merge、queue変更、alias配置による待機追加は、依存と寿命を変える。
変換後のグラフ・同期・配置を整合させ、古い解析結果をそのまま流用しない。
全体を無制限に反復するのではなく、各段の入出力と再解析条件を定義し、
必要なら保守的な配置へ戻れるようにする。公開例: Falcorは自動pass挿入後に順序を再解析する。
[Falcor compiler][falcor-compiler]
宣言順を保つ制約、途中挿入を禁じる制約、GPUメモリ依存もそれぞれ別の情報である。

<a id="synchronization"></a>

## 8. 同期とバックエンドへの具体化

### アクセス履歴から同期を導く

**API契約:** Vulkanではコマンドの投入順だけで必要なメモリ可視性は成立しない。
RAW／WAWには適切なメモリ依存、WARには実行依存が必要である。
read同士でもlayout変更や所有権移譲が必要になることはある。
[同期仕様][vk-sync]、[Synchronization Examples][vk-examples]

Graphの用途を、実際のstage／access／layoutへlowerする。例として、vertex属性はvertex shaderの
読み取りとは異なり、indexやindirect入力も固有の用途を持つ。
同じwriterを複数の異なるstageから読む場合、最初のreadだけを記録してwriterの可視化責任を失わない。
同期済みscopeと、まだ必要なconsumer scopeを表現できる状態追跡が必要になる。

同期計画のレビューでは、次を説明できるようにする。

- どのproducer／consumerの、どの資源・範囲の依存を満たすか。
- メモリ可視性、実行順、layout、queue ownershipのどれを変更するか。
- barrier、semaphore等が、どのnative位置・scopeで効くか。
- importされた予定状態と、次Graphへ渡す最終状態が整合するか。

**一般推奨:** 初期は保守的で正しい同期を基準とし、scopeを絞る場合は実際の全アクセスを保つ。
barrier件数の削減だけを目標にせず、GPU stallと並列性を測る。
同じ依存をglobal barrierへまとめる方式も、資源別barrierも、用途によって選べる。
画像layoutや所有権の変更をglobal memory barrierだけで置き換えない。
[Khronosのbarrier効率化例][vk-barriers]、[同期例][vk-examples]

### パス内・キュー間・hostの境界

**API契約:** Vulkan 1.3の通常のDynamic Renderingでは、追加featureなしにrendering scope内で
任意のbarrierを使えない。必要なパス間barrierをscope外に置く基準を守る。
[vkCmdPipelineBarrier2][vk-barrier-command]

別queueの依存には対応するqueue間同期が必要である。EXCLUSIVE資源がqueue familyを移る場合は
release／acquireも整合させる。同じfamilyの別queueでもアクセス依存は消えない。
[VkImageMemoryBarrier2][vk-image-barrier]

GPU処理後にCPUが読む場合は、host向けのメモリ依存、実GPU完了、非coherentメモリの
invalidate等を組み合わせる。CPUが読まない処理へ毎回readbackやCPU waitを挿入しない。
詳細な契約は[既存の同期ガイド](vulkan-best-practices.md#synchronization)を参照する。

<a id="external"></a>

## 9. import/export・履歴・Presentation

### 外部資源の引き渡し

import/exportはハンドルの受け渡しだけでは足りない。少なくとも次を契約化する。

| 情報 | 明確にする内容 |
|---|---|
| 同一性と所有 | 誰のどの物理資源か、誰が破棄するか、どの世代か |
| 有効内容 | どの範囲の値を利用できるか |
| 初期・最終状態 | 用途、native状態、queue ownershipを誰が確立するか |
| 先行仕事 | 利用開始に必要なGPU依存・完了tokenは何か |
| 外部への影響 | 外部観測されるwriteをcullingからどう保護するか |
| 更新と失効 | compile後の更新・削除・再importを検出するか、再計画するか |

公開例: Unityはフレーム・カメラ間で使うtextureを外部で所有してimportする。
Graph管理の一時textureとは寿命と解放責任が異なる。[Import a texture][unity-import]
外部資源への書き込みを自動的に副作用とするか、明示exportを要求するかは設計上の選択である。

単一queueでA→Bを投入する際、BはAの受理済みの予定状態を引き継げる。
これを実GPU完了と区別すると、不要なCPU待機を避けつつreadbackと回収の正しさを保てる。
複数queueや外部エンジンが介在する場合は、予定状態だけでなく実際の依存tokenも引き継ぐ。
submitされなかった計画の最終状態をcanonicalな状態として公開しない。

### 時間方向の依存

履歴画像は「前フレーム版を読む／今回版を書く」として扱い、単一フレームDAGの循環と区別する。
ping-pong／ringの枚数は、同時実行数と実際の最後の利用から決める。
固定の二重化だけでGPU使用中の上書きが防げるとは限らない。
リサイズ、format変更、カメラカット、露出・座標規約の変更等で履歴をいつ無効化するかも定義する。
CPU/GPUが共有内容を更新する際の競合回避は、[AppleのCPU/GPU同期例][metal-cpu]も参考になる。
上記の履歴無効化条件は本書の設計案である。

### 表示は独立した外部境界

acquireした画像をGraphへimportし、描画後は表示に必要な状態・同期とともに返す。
frame slotとSwapchain image indexを別々に管理する。
**API契約:** 描画submitのFence完了だけでは、Vulkan present待機semaphoreの使用終了を証明できない。
[Swapchain Semaphore Reuse][vk-present]

最小化、取得後キャンセル、`OUT_OF_DATE`、旧Swapchainの回収、終了はGraphの正常実行とは別の状態を持つ。
表示固有の回収根拠は[WSIガイド](vulkan-best-practices.md#wsi)へ委ね、
Graphの純粋な解析へウィンドウイベントループを持ち込まない。

<a id="lifetime"></a>

## 10. GPU寿命・失敗・再利用

次の寿命は一致しない。

1. Graph builderのCPUデータ。
2. compiled planと記録済み未投入命令が参照するデータ。
3. GPUへ投入してから最終利用が完了するまでのnative資源。
4. 完了後もCPUが保持するreadback結果。
5. 複数Graph／フレームに残る永続資源やキャッシュ。

**API契約:** Vulkan memoryを参照する提出済み命令の完了前に、そのmemoryを解放しない。
[vkFreeMemory][vk-free]
この条件を満たすため、画像だけでなくView、descriptor、pipeline、command資源、stagingと
allocationの依存を保持する。CPUのハンドルdropを即座のnative破棄としない。

再利用pool、upload ring、descriptor arenaは完了tokenで回収し、単なるframe番号に頼らない。
readback領域はGPU完了後もCPU消費まで保持し得る一方、command pool等は先に回収できる。
容量不足は、待機・拡張・要求拒否のどれを選ぶかを公開契約にする。pool容量とVRAM budgetは別である。

失敗経路は最低でも、宣言失敗、compile失敗、native記録失敗、未投入キャンセル、submit失敗、
timeout、Device lossを区別する。予約済み資源を返せる条件と、完了不明で保持すべき条件を記録する。
**API契約:** Vulkanのsubmit OOMの状態不変保証と、Device loss後の扱いはAPIごとの規則に従う。
[vkQueueSubmit2][vk-submit]、[Lost Device][vk-loss]

通常実行で全面idle待機へ依存せず、終了・異常時も含めて回収経路を用意する。
複数queueへ跨る資源は、全関連利用の完了が必要である。
異なるqueueの独立timeline値を、単に数値の最大値で一つの完了証明へまとめない。

<a id="aliasing"></a>

## 11. メモリ配置とエイリアシング

### 再利用とaliasingを区別する

同じ作成条件の画像・Viewを完了後にpoolから再使用することと、異なるresourceを同じ物理memoryへ
重ねて配置することは別の最適化である。前者だけでも毎フレームのnative作成を避けられる。
後者にはGPU上の生存期間とnativeメモリ要件の解析が追加で必要になる。

**API／allocator契約:** Vulkan／VMAのaliasingでは、size、alignment、memoryTypeBits、
dedicated条件、画像flags等の適合、切替時の同期と初期化を確認する。
`CAN_ALIAS`を指定するだけで寿命・同期・所有管理が実装されるわけではない。
共有allocationは単一の所有者が解放し、alias資源ごとに二重解放しない。
[VMA Resource aliasing][vma-alias]

### GPUの部分順序で安全性を証明する

単一queueの正しい順序ではfirst／last useの区間解析を使いやすい。
複数queueでは、任意に選んだ一つのトポロジカル順でAのlast useがBより前でも、GPU上では同時に動き得る。
同じ領域をAからBへ切り替えるなら、Aの全関連使用からBの競合使用へのhappens-beforeが必要である。
aliasingのために待機を追加する選択もあるが、削減メモリと失う並列性を計測する。
これは同期契約からの設計上の推論である。[同期仕様][vk-sync]、[VMA][vma-alias]

alias切替後のBを、Aの内容を保った初期化済み資源として扱わない。
**API契約:** D3D12ではaliasされたrender target／depth等を有効化するとき、
圧縮metadataの初期化条件も満たす。legacyのClear／Copy／Discardや、Enhanced Barriersの
対応するlayout遷移とDISCARD等、各経路の規則を確認する。
metadataの初期化と、shaderが読める値の確立は別であり、Discardだけで有効内容を作ったとはしない。
[Enhanced Barriers: Placed Resource Metadata Initialization][d3d-enhanced]
同期計画はresource handle単位だけでなく、重なる物理memoryも対象にする。
デバッグではaliasing無効、寿命延長、未初期化内容のpoisoningを比較できると原因を絞りやすい。
最適化OFFで成功しても、ONの構成の正しさを証明したことにはならない。

公開例: Atomはqueueを考慮した寿命の延長と、aliasing／asyncを無効にする比較経路を持つ。
これは一つの保守的な実装選択であり、queueを跨ぐaliasingが一般に不可能という意味ではない。
[Atom compiler][atom-compiler]

<a id="scheduling"></a>

## 12. スケジューリングと並列化

### async computeは目的関数を決めてから使う

依存を満たす順序は複数あり、CPU負担、GPU critical path、メモリピーク、latencyの最適順は一致しない。
compute queueへ移せることと、移すと速いことも別である。
shader core・cache・帯域の競合、queue間待機、submit数、frame間の重なりまで観測する。
Khronosの[async compute sample][vk-async]もGPU構造と負荷による違いを説明しており、
特にtile GPU向けの助言を全GPUへ外挿しない。

推奨する比較軸:

| 方針 | 得られる可能性 | 失う可能性 |
|---|---|---|
| producer直後にconsumerを配置 | 短い資源寿命、低いメモリピーク | 独立仕事との重なり |
| 独立Computeを別queueへ配置 | GPU利用率、critical pathの短縮 | 待機・帯域競合、長い資源寿命 |
| 多めのframes in flight | CPU/GPUの待機削減、throughput | 入力latency、資源複製とメモリ |
| aliasingのため順序を追加 | VRAMピークの削減 | 同時実行可能な仕事の直列化 |

計画から追加した辺・待機・queue選択理由を確認できるようにする。
単一queue経路を維持し、単なる「async有効」を最適化完了の根拠にしない。

公開例: RPSはprogram order保持、memory saving優先等の方針を表現する。
ただし確認版1.1.1の`ALLOW_SPLIT_BARRIERS_BIT`／`AVOID_RESCHEDULE_BIT`は将来用の予約である。
Graph上の並べ替え制約と、GPU memory barrierも別概念である。
[RPS header][rps-header]、[RPS subgraph tutorial][rps-subgraphs]

### CPU並列記録は別の問題

CPUの並列記録とGPUの複数queue実行は独立である。
記録jobの入力を固定し、共有可変状態を避け、workerと未完了実行単位ごとにpoolの所有を整理する。
**API契約:** Vulkan CommandPoolのhost同時利用やpending CommandBufferのreset条件を満たす。
小さすぎるsecondary command bufferは管理負担を増やし得るため、仕事量を測って分割する。
[Khronosの並列記録例][vk-record]
並列記録後もbarrierと各パス命令の正しい実行位置を維持する。
barrierだけを先頭のcommand listへ集め、後から全workerの命令を実行する変換は一般に正しくない。
[RPS並列記録tutorial][rps-record]

<a id="merging"></a>

## 13. パス統合・tile memory

Graphノード、エンジンの描画module、native render passは一対一でなくてよい。
統合はattachment互換性、load/store、resolve、必要な同期、queue、query等の境界を保つ変換として扱う。
単に隣接するRasterノードを結合してよいとは限らない。

公開例: UnityはRender Graph Viewerでmergeの状況を確認し、不要なcopyや過度に細かいパスを
避ける最適化手順を示している。[Optimize][unity-optimize]、[Viewer][unity-viewer]
本書ではmerge失敗理由を表示し、統合前後の結果と帯域・GPU時間を比較することを推奨する。

tile-local inputと任意座標samplingは別の用途である。
後続Computeやsamplingまで内容が必要な画像を、単に「一時画像」という理由でmemoryless扱いしない。
Vulkanのtransient attachment／lazily allocated memory、subpass、Dynamic Rendering local readは
それぞれ能力・usage・feature条件を持つ。Vulkan 1.3のDynamic Renderingだけからlocal read対応としない。
[Khronos Subpasses][vk-subpasses]、[Dynamic Rendering local read][vk-local-read]

load/storeの削減やtile内保持の利益はGPU依存であり、デスクトップとモバイルで同じ計画を強制しない。
統合を進めるとCPU記録並列性・debug粒度との交換条件も生じるため、目的に合わせる。

<a id="cpu"></a>

## 14. キャッシュとCPU負担

### 毎フレーム構築と計画キャッシュ

| 方式 | 向く状況 | 注意 |
|---|---|---|
| 毎フレーム宣言・compile | パス構成が動的、実装を単純に保ちたい | build／compileの費用を測り、payload複製や二次増加を避ける |
| 構造が変わるときだけcompile | 比較的固定した処理を反復する | 無効化条件と実行時binding・外部状態の更新が必要 |
| 共通解析のみ再利用しnative計画を更新 | 論理構造は安定し、import状態等が変動する | 複数段のcacheの依存関係が複雑になる |

この比較は設計上の選択であり、cache導入を必須とはしない。
公開例: Falcorの確認版は`mRecompile`が不要なら既存計画を使い、変更通知やresize等で再compileする。
FilamentにはGraphとは別のtexture cacheがある。[Falcor][falcor]、[Filament TextureCache][filament-cache]
cache keyには構造、用途・範囲、resource記述、外部契約、能力・queue方針、最適化設定等の
計画へ影響する入力を含める。毎フレーム変わる値は、計画に影響するものと単なるparameterへ分ける。
古いimport状態・物理世代・解像度で作ったbarrierやViewを再利用しない。
毎回native命令を記録することと、同じ記録済みCommandBufferを再提出することも別である。

### コピー・割り当て・保持を観測する

Graphのcompileで、upload本文やmeshデータを無条件にdeep cloneしない。
必要なら不変payloadを共有し、命令記録後に不要になるCPU本文をpending resultへ残さない。
ただし共有sliceが大きな元allocation全体を保持する場合は、保持byte数も計測する。
一時arenaのresetは、遅延実行が参照する全データの利用終了後に行う。

diagnostic dumpは要求時に生成でき、通常実行で巨大な文字列を常時構築しない設計が望ましい。
資源・View・pipeline・descriptor関連の再利用は、それぞれの互換性と完了条件で行う。
Graph計画cache、native object cache、driver pipeline cacheは別の仕組みである。

比較用の入力は、連続する大量のRaster、fan-out／fan-in、独立資源、重複範囲、
大量のdead branch、大きなupload等を含める。pass数だけでなくアクセス数、区間数、生成辺数、
allocation数、コピーbyte数、保持メモリ、dumpの費用を分けて測る。

<a id="portability"></a>

## 15. 移植性と拡張機能

### 共通化する意味と、API固有の状態

共通側は用途、範囲、依存、内容有効性、所有・保持条件を表し、backendから能力・メモリ要件・
記録上の制約を計画へ返せるようにする。Vulkanのlayout enumをそのまま共通APIの中心にしない。

| API | 共通化で失ってはいけない条件 |
|---|---|
| Vulkan | stage／access／layout、queue family ownership、semaphore scope、feature条件 |
| D3D12 legacy barriers | transition／UAV／aliasing barrier、state、promotion／decayの規則 |
| D3D12 Enhanced Barriers | sync／access／layoutの区分とqueue条件、対応能力の照会 |
| Metal | queue・API世代・hazard tracking mode・間接参照に応じた同期とresource使用宣言 |

**API契約:** D3D12 Enhanced Barriersは対応を確認して使う。legacy stateとの単純な一対一変換を
全用途に約束しない。現行仕様の追加機能をすべて最低要件にしない。
[Microsoft legacy barriers][d3d-legacy]、[Enhanced Barriers仕様][d3d-enhanced]

**API契約:** Metalも一律に「自動同期」としない。Appleは従来の`MTLCommandQueue`と
`MTL4CommandQueue`の条件を分けており、後者では`hazardTrackingMode`が作用しない。
対象OS・queue・resourceの作り方・直接／間接bindingを確認する。[Apple Resource synchronization][metal-sync]

公開例: AtomのScopeProducerは依存宣言、資源解決後の準備、命令記録を分け、
Executerはscopeと記録単位を分ける。これを特定のtrait構造まで模倣する必要はない。
[ScopeProducer][atom-producer]、[FrameGraphExecuter][atom-executer]

### bindless・indirect・native escape hatch

bindless descriptor、GPU address、indirect commandからの参照も資源依存を消さない。
参照候補集合を宣言する、所有arena全体を保守的に保持する、利用を制限する等の方針が必要である。
shader reflectionは型・bindingの検査を助けるが、実行時に選ばれる資源をすべて確定するものではない。
Graphが追跡できる範囲と利用者の責任を明文化する。

任意native命令を受け付ける場合も、読み書きする外部資源、前後の状態、queue、保持条件を宣言させる。
失う検証・並べ替え・mergeの保証を明記し、必要な範囲だけ保守的に同期する。
公開例: UnityのUnsafe passにもtextureアクセス宣言は必要で、通常のRaster passと同等の
最適化が保証されるわけではない。[Unsafe pass][unity-unsafe]
ここでいうUnsafe passはUnityの分類名であり、Rustの`unsafe`契約と同一ではない。

将来のray tracingではacceleration structure build／update／read、scratch、SBT等、
GPU-drivenではindirect／count buffer等の実際の用途・保持条件をモデルへ追加する。
未対応機能を一般的な「shader read/write」だけに隠して対応済みとしない。
これらは将来の設計時に調査を追加する境界であり、本書は各拡張の実装仕様ではない。

<a id="verification"></a>

## 16. 診断・検証・性能測定

製品・開発用の分類には[検証方針](validation-policy.md)を適用する。
入力の保証を上流へ移し、製品では保証済み内容の反復scanと開発用の専用metadata保持を避ける。
producer・hazard・culling・native同期等の計画作成と、結果を独立に再点検する診断を分離する。
safeな計画を成立させる拒否や実行時に変わる資源状態の処理を、診断と一括して無効にしない。

### 診断を実行計画の一部として設計する

passとresourceへ安定したIDと読める名前を付け、宣言元からnative object／command labelまで追跡する。
大規模なDAG図だけに頼らず、用途に応じて次を表示・dumpする。

- pass×resourceのアクセス表、producer／version、初期化済み範囲。
- cullingされたpassと、その理由、保持された副作用。
- queue別timeline、wait／signal、barrierのscopeと生成理由。
- 論理寿命と物理allocation配置、alias切替、外部資源・履歴の境界。
- compile変換前後、cache hit／無効化理由、作成・再利用・保持byte数。

公開例: Unity Viewerはアクセス・merge等を可視化し、Unreal RDGには個別の最適化を無効化する
診断経路がある。[Unity Viewer][unity-viewer]、[Unreal RDG][unreal]
本書では、通常実行の診断費用を制御し、調査時に詳細化する構造を推奨する。

### CPU解析テスト

以下は本書の試験案であり、固定件数を要求するものではない。
単純な参照モデルや性質テストを使い、実装の内部構造の再現だけにならないようにする。

| 分野 | 行動・契約として確認する例 |
|---|---|
| dataflow | 上書き前後のproducer、部分writeの穴、read_write、別versionの利用 |
| culling | 未使用branch除去、外部副作用保持、dead writer除去後にも残るhazard |
| 順序 | RAW／WAR／WAW、独立read、明示依存、循環と再現可能な順序 |
| attachment | Load前の初期化、部分clear、Discard後read、depth／stencil／resolve |
| 同一性 | 別Graph／runtime、重複View・import、削除後の世代、compile後の更新 |
| 時間方向 | 前回版／今回版、未完了Graphの引き継ぎ、履歴失効、未投入計画の破棄 |
| 変換 | merge／alias／queue配置／自動pass挿入後も出力・依存が保存される |
| 負荷 | 大量pass、区間の分割・統合、fan-out、容量不足時の診断 |

有用な性質の例は、独立パスの合法な順序変更で観測結果が変わらないこと、不要branchの追加が
必要出力を変えないこと、最適化ON/OFFで同じ意味の出力を得ることである。
浮動小数点reduction等で順序を固定すべき処理は、先にその契約を明示する。
検証専用に合法順序を変える試験は隠れた依存の発見に使えるが、無作為試験だけで安全性は証明しない。

### 実GPUと失敗経路

画像・byteの照合、Validation、実際の完了と回収の確認を組み合わせる。
readbackの順不同待機、timeout後再待機、更新・削除・容量不足、記録失敗・未投入キャンセル、
複数frame、リサイズ・最小化・終了等を優先する。
backend間では同じ論理グラフで結果を比較し、色・深度・座標・浮動小数点の差を明記する。
実機で再現が難しいnative OOMやDevice lossは、境界の限定的なfault injectionと実機確認を区別する。

**重要な検出限界:** SyncValにはaliasing、間接アクセス、host memoryアクセス等の制限がある。
検証メッセージがゼロでも、未初期化内容、寿命、alias配置、shaderの正しさをすべて保証しない。
使用SDK版の制限を確認し、構造テストと内容照合を維持する。[LunarG SyncVal][syncval]

本Rendererの実GPU試験はCPU試験と分け、`#[ignore]`を明示して実行する。
必要環境が欠ける場合は失敗し、Validation warning／errorゼロを要求する。
確認済み誤検出だけを個別に記録し、ログは残す。[開発ガイド](development.md#実vulkan試験)

### 性能の目的と測定条件

CPUのbuild／compile／record／submit／回収、GPUのpass時間・critical path、
CPU/GPU間latency、VRAMと一時メモリピーク、CPU allocation・copy量を分離する。
GPU queryの取得のために毎フレーム待機して、通常実行の性質を変えない。

release、検証の実効設定、GPU／driver／OS、解像度、scene・seed、機能設定、warm-up、
測定回数、中央値・上位percentileを保存する。検証有効の正しさ試験と性能試験を分ける。
最適化の比較は同じ出力・負荷で行い、平均FPSだけで判断しない。
他エンジンの数値やベンダーsampleの改善率を本Rendererの成果として転用しない。

<a id="decisions"></a>

## 17. 設計選択とレビュー基準

### 方式を選ぶときの判断表

| 判断 | 推奨する出発点 | 別方式を選ぶ材料 |
|---|---|---|
| versioning | 各readのproducerを一意に説明できる方式 | APIの扱いやすさ、部分write、履歴・並べ替えの要求 |
| 解析粒度 | 対応する用途に必要な範囲を正しく追う | 部分mip等の並列性と区間管理費用の計測 |
| Graph更新 | 単純で検証しやすい方式 | compileがボトルネックなら計画再利用と失効契約 |
| native同期 | 正しい保守的経路 | 実際のscope・stallを調べたbarrier削減 |
| 資源再利用 | 完了を条件とするpool・cache | 容量、互換性、保持メモリ、作成費用 |
| 物理aliasing | 無効の比較基準を維持 | ピークVRAM削減が同期・複雑さの費用に見合う |
| queue配置 | 単一queueの基準 | critical path、帯域競合、待機、latencyの実測 |
| CPU記録 | 十分な仕事量の単位 | worker数、記録費用、pool競合、native API制約 |
| パス統合 | 論理passの意味を保つ | tile帯域、load/store、記録並列性の比較 |
| 共通API | 用途・依存・寿命を共通化 | 第二backendの具体的な制約と能力差 |

「全部の高度機能がある」「barrierが少ない」「ノードが少ない」こと自体は品質の証明にならない。
変更ごとに、解決する問題、代替案、保証する不変条件、検証方法、利益と悪化を説明する。

### レビューで答える問い

1. すべての実アクセス・副作用を宣言し、各readのproducerを説明できるか。
2. 同一物理資源・View・aliasの重複を見逃していないか。
3. culling・並べ替え・merge後も内容と必要なhazardが保存されるか。
4. layoutとは別に、読む範囲の有効内容を証明できるか。
5. 同期のscopeとqueue依存を実際の命令へ結び付けられるか。
6. 未投入、未完了、完了後CPU保持の各寿命を区別しているか。
7. 外部資源・履歴・表示の状態と所有を正しく引き継げるか。
8. cacheの無効化、容量不足、失敗後の回収が定義されているか。
9. 最適化の比較基準・診断経路があり、対象機材で利益を測ったか。
10. backend固有のAPI契約を共通化の都合で落としていないか。

<a id="application"></a>

## 18. 本Rendererへの適用

本書によって現行APIやロードマップを変更しない。
現在の単一queue、物理aliasingなし、固定操作、buffer範囲の追跡、厳格な検証は、
検証可能な基準を保つ選択である。一般推奨が将来機能へ言及していても、直ちに実装する義務はない。

| 段階 | 本書から確認する領域 |
|---|---|
| M2の維持・改善 | producerとhazardの分離、内容有効性、解析量、CPU payloadの保持、native同期 |
| M3 | GPU完了による再利用、永続資源・履歴、エンジン入力とカスタムパス、acquire/presentと異常経路 |
| M4 | 第二APIで共通意味論を実証、PassEncoderの保証、aliasingと複数queueの小実験 |
| M5以降 | 計測に基づく描画機能・aliasing・queue配置・並列記録・merge・cacheの拡張 |

可変ハンドルと宣言順のproducer追跡は、明示version方式と比較すべき設計上の選択であり、
明示versionを使わないだけで誤設計とはしない。
現行の全体native buffer barrierも、論理byte範囲解析と別の保守的な粒度である。
詳細は[現在の契約](../design/rendergraph.md)で確認する。

新機能や一般推奨から異なる選択は、関連する既存設計文書とBeadsに理由・代替案・リスク・根拠を残す。
変更の受入条件、実行した／していない確認、対応環境を記録する。
このガイドの調査は、実装の完全な監査、GPU実行、最適化の性能実証を兼ねない。

<a id="sources"></a>

## 一次資料と更新方法

### エンジン資料・公開実装

確認版は「調査に用いた版」であり、最新リリースの保証ではない。
ソースは以下のcommitへ固定し、動的な公式ガイドと区別する。

| 資料 | 確認版・読んだ範囲 |
|---|---|
| [Frostbite FrameGraph][frostbite] | GDC 2017、EA/DICE投稿の63枚のスライド本文。旧EA記事は404、講演動画は未視聴 |
| [Unreal RDG][unreal] | 調査時5.9表記の動的ページ。setup/execute、依存、外部資源、検証・診断 |
| [Unity概要][unity-intro]／[パス作成][unity-pass]／[import][unity-import] | Unity 6000.0、宣言と実行、textureの所有・寿命 |
| [Unity最適化][unity-optimize]／[Unsafe][unity-unsafe]／[Viewer][unity-viewer] | Unity 6000.0、merge、escape hatch、アクセス・診断表示 |
| [Filament notes][filament] | 動的な設計ノート。細部は次の固定ソースと区別 |
| [Filament FrameGraph.cpp][filament-source]／[TextureCache.cpp][filament-cache] | v1.76.1、`65eaf091d7236b64094c59354c8632a86b911f92`。version、culling、寿命とcache |
| [Atom docs][atom]／[compiler][atom-compiler]／[producer][atom-producer]／[executer][atom-executer] | O3DE 2605.0、`3db6943249d8bd7960b9ed7e9aee310b7668586e`。共通／固有compile、queueと寿命、記録単位 |
| [RPS][rps]／[header][rps-header]／[FAQ][rps-faq]／[subgraphs][rps-subgraphs]／[並列記録][rps-record] | Open Beta 1.1.1、`f3330f5306d15af8529a310f6255225c864b0961`。方針flag、完了通知、並列化 |
| [Falcor Graph][falcor]／[compiler][falcor-compiler]／[pass資料][falcor-passes] | 9.0、`759aad033ff610fb0d82c74f7e0a508d0096d5f2`。dirty compile、依存、履歴の制約 |

Falcorのfield名のaliasは同じresource entryへの名前の対応であり、その記述だけを
物理heap aliasingの証拠にしない。[ResourceCache.cpp][falcor-cache]

補足: Falcorは変換でresolve等のpassが追加されると順序を再解析する実装例になる。
またpass資料のpersistent指定は、再compileを跨ぐ内容保持まで保証しない。
cacheや履歴の保証は、用語だけから推測しない。[compiler][falcor-compiler]、[pass資料][falcor-passes]
RPSもframe番号と実GPU完了を区別して回収を通知する。[FAQ][rps-faq]
これらのエンジンをこの調査環境でビルド・実行したわけではない。

### native API・allocator・検証の根拠

| 資料 | 主に確認した契約・助言 |
|---|---|
| [Vulkan同期仕様][vk-sync]／[同期例][vk-examples] | 実行依存とメモリ依存、用途別scope |
| [Image barrier][vk-image-barrier]／[barrier命令][vk-barrier-command] | subresource、所有権、rendering scope内の制限 |
| [Load][load-op]／[Store][store-op]／[Layout][image-layout] | attachmentと未定義内容の区別 |
| [FreeMemory][vk-free]／[QueueSubmit2][vk-submit]／[Device loss][vk-loss] | 完了、未投入失敗、終端状態 |
| [VMA aliasing][vma-alias] | 配置の適合、非重複使用、切替時の同期・初期化 |
| [barrier効率][vk-barriers]／[async compute][vk-async]／[並列記録][vk-record] | 条件付き最適化の比較材料 |
| [Subpasses][vk-subpasses]／[local read][vk-local-read] | tile-localな用途と追加機能の条件 |
| [Buffer Device Address][bda]／[Swapchain semaphore][vk-present] | 間接参照と表示境界 |
| [D3D12 legacy][d3d-legacy]／[Enhanced Barriers][d3d-enhanced] | APIごとの状態・同期モデル、能力条件 |
| [Metal resource同期][metal-sync]／[CPU/GPU同期][metal-cpu] | queue世代・hazard modeとCPU更新の競合 |
| [LunarG SyncVal][syncval] | 検出対象と限界。使用SDK版でも照合する |

Webの`latest`資料・Apple／Microsoftの動的資料は更新される。
実装時は固定SDK、enabled feature、ash／VMA版、対象OS／GPUと照合し、
根拠が変わった節をこのガイド内で更新する。日付別の調査文書を増やさない。
未閲覧資料の推測、未確認の性能主張、予約flagを実装済みとする説明は根拠へ加えない。

[frostbite]: https://www.slideshare.net/slideshow/framegraph-extensible-rendering-architecture-in-frostbite/72795495
[unreal]: https://dev.epicgames.com/documentation/unreal-engine/render-dependency-graph-in-unreal-engine
[unity-intro]: https://docs.unity.com/en-us/engine/6000.0/manual/render-pipelines/universal-render-pipeline/customizing-urp/render-graph/introduction
[unity-pass]: https://docs.unity.com/en-us/engine/6000.0/manual/render-pipelines/universal-render-pipeline/customizing-urp/render-graph/write-render-pass
[unity-import]: https://docs.unity.com/en-us/engine/6000.0/manual/render-pipelines/universal-render-pipeline/customizing-urp/render-graph/working-with-textures/pass-textures-between-passes/import-a-texture
[unity-optimize]: https://docs.unity.com/en-us/engine/6000.0/manual/render-pipelines/universal-render-pipeline/customizing-urp/render-graph/optimize
[unity-unsafe]: https://docs.unity.com/en-us/engine/6000.0/manual/render-pipelines/universal-render-pipeline/customizing-urp/render-graph/unsafe-pass
[unity-viewer]: https://docs.unity.com/en-us/engine/6000.0/manual/render-pipelines/universal-render-pipeline/customizing-urp/render-graph/viewer-reference
[filament]: https://google.github.io/filament/notes/framegraph.html
[filament-source]: https://github.com/google/filament/blob/65eaf091d7236b64094c59354c8632a86b911f92/filament/src/fg/FrameGraph.cpp
[filament-cache]: https://github.com/google/filament/blob/65eaf091d7236b64094c59354c8632a86b911f92/filament/src/TextureCache.cpp
[atom]: https://docs.o3de.org/docs/api/gems/atom/class_a_z_1_1_r_h_i_1_1_frame_graph
[atom-compiler]: https://github.com/o3de/o3de/blob/3db6943249d8bd7960b9ed7e9aee310b7668586e/Gems/Atom/RHI/Code/Source/RHI/FrameGraphCompiler.cpp
[atom-producer]: https://github.com/o3de/o3de/blob/3db6943249d8bd7960b9ed7e9aee310b7668586e/Gems/Atom/RHI/Code/Include/Atom/RHI/ScopeProducer.h
[atom-executer]: https://github.com/o3de/o3de/blob/3db6943249d8bd7960b9ed7e9aee310b7668586e/Gems/Atom/RHI/Code/Include/Atom/RHI/FrameGraphExecuter.h
[rps]: https://github.com/GPUOpen-LibrariesAndSDKs/RenderPipelineShaders/tree/f3330f5306d15af8529a310f6255225c864b0961
[rps-header]: https://github.com/GPUOpen-LibrariesAndSDKs/RenderPipelineShaders/blob/f3330f5306d15af8529a310f6255225c864b0961/include/rps/runtime/common/rps_runtime.h
[rps-faq]: https://github.com/GPUOpen-LibrariesAndSDKs/RenderPipelineShaders/blob/f3330f5306d15af8529a310f6255225c864b0961/docs/faq.md
[rps-subgraphs]: https://github.com/GPUOpen-LibrariesAndSDKs/RenderPipelineShaders/blob/f3330f5306d15af8529a310f6255225c864b0961/docs/tutorial/rps_tutorial_p2.md
[rps-record]: https://github.com/GPUOpen-LibrariesAndSDKs/RenderPipelineShaders/blob/f3330f5306d15af8529a310f6255225c864b0961/docs/tutorial/rps_tutorial_p4.md
[falcor]: https://github.com/NVIDIAGameWorks/Falcor/blob/759aad033ff610fb0d82c74f7e0a508d0096d5f2/Source/Falcor/RenderGraph/RenderGraph.cpp
[falcor-compiler]: https://github.com/NVIDIAGameWorks/Falcor/blob/759aad033ff610fb0d82c74f7e0a508d0096d5f2/Source/Falcor/RenderGraph/RenderGraphCompiler.cpp
[falcor-cache]: https://github.com/NVIDIAGameWorks/Falcor/blob/759aad033ff610fb0d82c74f7e0a508d0096d5f2/Source/Falcor/RenderGraph/ResourceCache.cpp
[falcor-passes]: https://github.com/NVIDIAGameWorks/Falcor/blob/759aad033ff610fb0d82c74f7e0a508d0096d5f2/docs/usage/render-passes.md
[vk-sync]: https://docs.vulkan.org/spec/latest/chapters/synchronization.html#synchronization-dependencies
[vk-examples]: https://docs.vulkan.org/guide/latest/synchronization_examples.html
[vk-image-barrier]: https://docs.vulkan.org/refpages/latest/refpages/source/VkImageMemoryBarrier2.html
[vk-barrier-command]: https://docs.vulkan.org/refpages/latest/refpages/source/vkCmdPipelineBarrier2.html
[load-op]: https://docs.vulkan.org/refpages/latest/refpages/source/VkAttachmentLoadOp.html
[store-op]: https://docs.vulkan.org/refpages/latest/refpages/source/VkAttachmentStoreOp.html
[image-layout]: https://docs.vulkan.org/refpages/latest/refpages/source/VkImageLayout.html
[vk-free]: https://docs.vulkan.org/refpages/latest/refpages/source/vkFreeMemory.html
[vk-submit]: https://docs.vulkan.org/refpages/latest/refpages/source/vkQueueSubmit2.html
[vk-loss]: https://docs.vulkan.org/spec/latest/chapters/devsandqueues.html#devsandqueues-lost-device
[vma-alias]: https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/resource_aliasing.html
[vk-barriers]: https://docs.vulkan.org/samples/latest/samples/performance/pipeline_barriers/README.html
[vk-async]: https://docs.vulkan.org/samples/latest/samples/performance/async_compute/README.html
[vk-record]: https://docs.vulkan.org/samples/latest/samples/performance/command_buffer_usage/README.html
[vk-subpasses]: https://docs.vulkan.org/samples/latest/samples/performance/subpasses/README.html
[vk-local-read]: https://docs.vulkan.org/samples/latest/samples/extensions/dynamic_rendering_local_read/README.html
[bda]: https://docs.vulkan.org/guide/latest/buffer_device_address.html
[vk-present]: https://docs.vulkan.org/guide/latest/swapchain_semaphore_reuse.html
[d3d-legacy]: https://learn.microsoft.com/en-us/windows/win32/direct3d12/using-resource-barriers-to-synchronize-resource-states-in-direct3d-12
[d3d-enhanced]: https://microsoft.github.io/DirectX-Specs/d3d/D3D12EnhancedBarriers.html
[metal-sync]: https://developer.apple.com/documentation/metal/resource-synchronization.md
[metal-cpu]: https://developer.apple.com/documentation/metal/synchronizing-cpu-and-gpu-work.md
[syncval]: https://vulkan.lunarg.com/doc/view/latest/windows/synchronization_usage.html
