# RenderGraph設計図集

構造・依存・状態・寿命を、別々の問いとして読むための図集です。
最初に[RenderGraphの案内](rendergraph-overview.md)、正確なAPI制約は[詳細設計](rendergraph.md)を参照してください。
図は実装確認点[`adb51d1`](https://github.com/tsukushibito/renderer/tree/adb51d17e3c1f43b380163a444f750ee9ff9edf5)に対応します。
未信頼shader・一般encoder・複数queue・物理aliasingを、実装済みとして描いていません。

| 知りたいこと | 読む図 |
|---|---|
| 利用側、共通Graph、backendは何を担当するか | [1. 全体構造](#architecture) |
| GraphPoolとGraph、planは何を所有するか | [2. 主要Rust型の所有](#ownership) |
| owned Rasterのdraw・attachment・Programはどこにあるか | [3. passとarena](#owned-pass) |
| bindingの用途とshader stageは誰が決めるか | [4. bindingとstage](#binding-stages) |
| compileは何をどの順で計算するか | [5. compileの流れ](#compile) |
| 上書きされた処理をどう取り除くか | [6. producerとculling](#producer-culling) |
| RAW・WAR・WAWは何を制約するか | [7. live hazard](#hazards) |
| bufferの一部や画像mipをどう区別するか | [8. 範囲とsubresource](#ranges) |
| submitからCPU結果を得るまでに何が起きるか | [9. native実行のsequence](#native-sequence) |
| GPUが終わると何を返却できるか | [10. 別々の寿命](#lifetimes) |
| timeoutや未投入cancelはどの状態に戻るか | [11. workとresultの状態](#work-result-state) |
| snapshot、予定状態、Fenceはどう関係するか | [12. importと内容revision](#import-revision) |
| 容量が足りないとき誰が何を決めるか | [13. 容量とbackpressure](#capacity) |

各図のMermaid原本は`rendergraph-diagrams/*.mmd`です。
案内と図集は同じ原本をincludeします。図には支援技術向けの`accTitle`と`accDescr`を付けています。
Mermaidの[アクセシビリティ仕様](https://mermaid.js.org/config/accessibility.html)に従い、
図だけで契約を伝えず、直後の文章に前提・制限・ソースを示します。

<a id="architecture"></a>

## 1. 全体構造：共通計画からbackendへ

```mermaid
{{#include rendergraph-diagrams/architecture.mmd}}
```

Graph構築とcompileはGPU・windowなしで使えます。`CompiledGraph`から先がnative実行です。
共通Graphが成功しても、選択したbackendがすべての操作を実行できるとは限りません。
liveな未対応操作は明示エラーにし、除去された枝だけの物理資源は取得しません。

Vulkanは単一Graphics＋Compute queueで限定した描画・Compute・copy等を実行します。
D3D12の公開Graph実行はtransient bufferと画像のupload/copy/readback、owned Raster/Computeです。
ソース：[Graph facade](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph.rs)、
[Vulkan runtime](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/backends/vulkan/graph.rs)、
[D3D12 runtime](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/backends/d3d12/graph.rs)。

<a id="ownership"></a>

## 2. 主要Rust型：実内包とArc pinを区別する

```mermaid
{{#include rendergraph-diagrams/ownership.mmd}}
```

このclass図は実装にある型の関係を抜粋し、全fieldを列挙したものではありません。
黒い菱形の`*--`だけが値の実内包です。実線の矢印はラベルに記した共有保持、
破線は作成・消費変換または識別関係です。継承関係はありません。

`GraphPool`の`Vec<Arc<Slot>>`と、`Graph`または`CompiledGraph`の`Arc<Slot>`が同じbackingを保ちます。
poolがslotを排他的に再取得できるまで書き換えません。`compile(self)`はGraphを消費するため、
可変builderと不変planが同時に同じ領域を書き換える経路を作りません。
handleはidentityとindexの値で、GPU資源やslotへの強い参照ではありません。

ソース：[slotとpool](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/storage.rs)、
[CompiledGraph](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/compile.rs)、
[handleとView](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/resource.rs)。

<a id="owned-pass"></a>

## 3. passとarena：命令は範囲で参照する

```mermaid
{{#include rendergraph-diagrams/owned-pass.mmd}}
```

配列を実所有するのは`GraphStorage`です。`Pass`は`Operation`を値で持ち、
Operation内の`draws`・`colors`、Dispatchの`bindings`はarenaの範囲、`depth`は任意のindexです。
各`RasterDrawOperands`もdrawごとの`bindings`範囲を持ち、descriptorを共有のarenaへ保持します。
したがって、Operationがdraw用Vecやattachment用Vecを個別に所有する図にはしません。
借用した入力sliceから必要な値を構築時にcaptureし、短命な呼出元をplanの寿命へ持ち込みません。

Rasterは`ManagedRasterProgram`、Dispatchは別の`ManagedProgram`の値を持ちます。
どちらも不変static descriptorに結び付くCPU契約で、native PSOの所有型ではありません。
`BindingOperands`はBuffer・ImageView・Samplerの列挙型です。Bufferは論理資源indexと完全なdescriptor範囲、
Viewは論理画像のsubresource値、Samplerは不変設定を持ち、native資源の所有型ではありません。
`mesh()`はbindingを持たず、`textured_mesh()`はdrawごとのUniform・sampled View・SamplerをCPUで計画します。

現在のRasterは複数drawを同じLoad/Storeの下で保持します。色ProgramはRGBA8またはRGBA16F一個と任意depth、
深度専用Programはcolorなし＋D32を使います。両native backendとも各1 subresourceです。
指定Load/Storeは保持し、空drawはattachment-onlyへ正規化します。
`textured_mesh()`のdraw別bindingも明示準備したnative PSO/descriptorへ接続します。
`Graph::dispatch`も明示準備したCompute pipeline/descriptorから実行します。一般encoderと未信頼shaderは未対応です。
ソース：[GraphStorage](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/storage.rs)、
[OperationとPass](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/pass.rs)、
[Raster builder](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/builder.rs)、
[Raster Program](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/program/raster.rs)。

<a id="binding-stages"></a>

## 4. bindingとstage：Programが用途を決める

```mermaid
{{#include rendergraph-diagrams/binding-stages.mmd}}
```

公開入力は`graph::Binding { group, binding, resource }`です。
`BindingResource`はBuffer・ImageView・Samplerで、旧`BufferBinding`入口はありません。
native資源の作成APIにある別型の`program::Binding`とは区別してください。
構築時にProgram layoutとのslot・型・ABIを照合し、manifest順の`BindingOperands`と、
実際の読書き範囲を示す`ResourceAccess`を別々に保持します。
Samplerは設定をcaptureしますが、内容accessやproducer辺を作りません。

| 対象 | `ResourceAccess`の範囲・用途・stage |
|---|---|
| ProgramのBuffer binding | レビュー済みbyte footprint、Uniform/Storage用途、manifestの`ShaderStages` |
| Programのsampled View | 選択mip/layer全域、Sampled用途、manifestの`ShaderStages` |
| Programのstorage View | typed formatとdispatch/extentに結び付く選択mip/layer全域、StorageRead/Write用途、COMPUTE |
| vertex/index buffer・attachment | operand由来のVertex/Index/ColorAttachment/DepthAttachment用途。shader stageとは独立 |

`textured_mesh()`はVERTEX Uniform16 byteと、FRAGMENT sampled View＋nearest/clampを要求します。
頂点stride28・inline行列＋tint80 byte・1 RGBA8＋任意depthは`mesh()`と同じCPU契約です。
liveなtextured Rasterは両APIで実行し、未準備のProgramやdescriptor容量不足は明示エラーにします。
`mesh_hdr()`/`textured_mesh_hdr()`はRGBA16F出力、`depth_only()`はfragment stageなしの深度出力です。
Rasterのstageとshaderは引き続きレビュー済みassetから導きます。

Dispatchの管理下assetは`uniform_readback()`、`storage_readback()`、`sampled_readback()`、
`sampled_float_readback()`の4つで、全てworkgroups `[1,1,1]`です。
Storage出力の先頭16/16/16/64 byteだけを確立し、sampled入力は選択subresource全域を保守的にreadします。
全bindingのshader stageはCOMPUTEです。descriptorの残りを初期化済みとはしません。
利用側は不変descriptorのunsafe取込でComputeを追加でき、型付きstorage画像とextent由来dispatchを扱えます。
shaderの全域write保証とbinding範囲をレビューし、両native backendが明示準備したpipeline/descriptorを使います。

ソース：[Bindingと正規化・access導出](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/dispatch.rs)、
[ShaderStagesとBindingLayout](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/program.rs)、
[管理下Raster manifest](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/program/raster.rs)、
[native能力境界](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/backends/vulkan/graph/work.rs)。

<a id="compile"></a>

## 5. compile：内容、生存、順序を順に求める

```mermaid
{{#include rendergraph-diagrams/compile.mmd}}
```

owner・範囲・用途等は構築境界で保証します。compileでもproducer、未初期化read、
culling、live hazard、容量、循環、最終内容の確定は必要な実行計画処理として残ります。
builder失敗は追加途中のarenaを戻し、compile失敗は不完全な計画を公開しません。
生存起点はexportが要求するproducer、readback、明示的副作用、import資源へのwriteです。

`development-validation`は保証済み宣言の独立再点検等を加えます。
liveな循環は常に拒否し、dead部分だけの循環は開発構成で追加診断します。
全passが宣言順のまま残る場合の再計算省略等、同じ意味を保つ短絡経路は図ではまとめています。
ソース：[共通解析の入口](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/analysis.rs)、
[compileの凍結とエラー変換](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/compile.rs)。

<a id="producer-culling"></a>

## 6. producerとculling：順序制約で不要passを生かさない

```mermaid
{{#include rendergraph-diagrams/producer-culling.mmd}}
```

[案内の実行例](rendergraph-overview.md#2-小さなgraphを作ってみる)に対応します。
P1は古い内容をscratchへcopyしますが、その結果を使うpassはありません。
観測するP3はP2の新しい内容を読むため、P0とP1は除去されます。
破線のWAW・WARは順序制約であり、その辺だけをproducerとして逆に辿りません。

readの内容は宣言順で決まります。明示依存を後から追加しても、readが別のwriterを読むようには変わりません。
明示依存の先は意味的な前提として残します。未初期化readは除去前に検査します。
ソース：[dataflow・生存解析](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/analysis.rs)、
[明示依存の契約](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph.rs)。

<a id="hazards"></a>

## 7. live hazard：同じ範囲の利用を安全に並べる

```mermaid
{{#include rendergraph-diagrams/hazards.mmd}}
```

三つは独立した例です。各passは別の観測結果・明示依存等によって既にliveであり、
同じ資源の重なる範囲を使う前提です。非重複範囲や別資源まで一律に競合扱いしません。
writeがあればRAW・WAR・WAWを求め、read/readだけからhazard辺は作りません。

backendではRAWとWAWに適切なmemory dependency、WARに必要なexecution dependencyを記録します。
画像layout変更等の条件は別途必要であり、read/readだからnative状態も無条件に同じとは限りません。
Graphのtopological順だけでは、GPUの書き込みの可視性は成立しません。
ソース：[hazard解析](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/analysis.rs)、
契約：[同期のガイド](../guides/vulkan-best-practices.md#synchronization)。

<a id="ranges"></a>

## 8. 範囲とsubresource：一部の初期化を全体へ広げない

```mermaid
{{#include rendergraph-diagrams/ranges.mmd}}
```

bufferは半開byte範囲、画像はmip/layer/aspectで内容を扱います。
画像Viewは同じImageHandleへ戻る値で、新しい物理画像の作成ではありません。
exportは全域に有効内容を要求するため、図の画像はmip0だけのclearではexportできません。
CPUのsubresource解析と、nativeが複数mip/layer画像を実行できるかは別の能力です。

内容の有効性は「読む値が定義されているか」、layoutは「native用途に適した状態か」、
正常完了は「GPUが成功して終わったか」です。画像が適切なlayoutでも内容が有効とは限らず、
有効な予定内容があってもCPUから即座に読めるとは限りません。
ソース：[資源・View](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/resource.rs)、
[区間解析](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/analysis/intervals.rs)。

<a id="native-sequence"></a>

## 9. native実行：投入受理と正常完了の間を待つ

```mermaid
{{#include rendergraph-diagrams/native-sequence.mmd}}
```

Vulkanの`submit`/`Submission::wait`経路の例です。native submit成功後に受理済み予定状態を公開します。
`wait`のtimeoutは`None`で、同じSubmissionを保持して再試行できます。
正常完了だけがreadbackの有効性を確立し、`readback_into`成功でそのstaging範囲を返します。
別Graphのhandleと二重消費は拒否します。

図は成功とtimeoutに絞っています。native OOM、Device loss、完了不明の失敗の保持・停止は
[GPU状態と所有](rendergraph.md#gpu状態と所有)に従います。
`submit_frame`ではruntimeが上限付きpendingを所有し、利用側は`poll_frame`/`wait_frame`で完了を受け取ります。
ソース：[submitとSubmission](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/backends/vulkan/graph.rs)、
[正常完了後の保持の整理](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/backends/vulkan/graph/work/record.rs)。

<a id="lifetimes"></a>

## 10. 別々の寿命：実行枠・plan・CPU結果

```mermaid
{{#include rendergraph-diagrams/lifetimes.mmd}}
```

図の矢印は必要な条件の関係で、すべての所有が一本の時系列で終わるという意味ではありません。
正常完了後にnative実行枠や記録用参照を回収しても、未読readbackはCPU結果に残せます。
Graph slotの再取得条件は、利用側のcloneやnative/provenance等を含む**全plan pinの返却**です。
未読readbackが常にplanを保持する、とも、GPUが終われば利用側cloneが無効になる、とも扱いません。

export物理資源の登録解除は別の明示`delete`です。snapshotのDropだけでは登録を解除せず、
deleteしてもpending側が物理参照を持つ間はGPU使用中の資源を破棄しません。
ソース：[pool再取得](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/storage.rs)、
[GraphWorkの保持](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/backends/vulkan/graph/work.rs)、
契約：[slot所有](rendergraph.md#prepared-graph-storage)。

<a id="work-result-state"></a>

## 11. workとresultの状態：timeoutでは返却しない

```mermaid
{{#include rendergraph-diagrams/work-result-state.mmd}}
```

これはnative所有契約の概念図であり、同名の単一Rust enumが実装されている図ではありません。
未投入であると証明できる取消と、投入後に正常完了を観測した退役を区別します。
timeoutはpendingの自己遷移で、未完了の所有を維持します。
Device loss等で完了が不明なら、通常の再利用へ戻さず停止・保持の規則に従います。

完了済みCPU結果を破棄する場合の領域返却と、pending処理のDropも同じではありません。
D3D12の公開`prepare`は未投入command・資源を所有し、投入後の所有はSubmissionへ移ります。
ソース：[Vulkan完了管理](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/backends/vulkan/transfer.rs)、
[D3D12 Graphのprepared/submission/result](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/backends/d3d12/graph.rs)。

<a id="import-revision"></a>

## 12. importと内容revision：予定状態を完了と取り違えない

```mermaid
{{#include rendergraph-diagrams/import-revision.mmd}}
```

Vulkanのimport snapshotはowner・世代・内容revision・desc・初期化情報を表します。
snapshotが古いstage/access/layoutを固定して再生する構造ではありません。
投入時にruntimeのcanonical予定状態から依存を計画し、別runtimeや古い内容revisionを拒否します。
write・clear・discardでrevisionを進め、readやlayout変更だけでは進めません。

A受理後のfresh snapshotからBを構築すれば、同じqueueへAのCPU待機なしで投入できます。
GPU間の必要な依存と、CPU readback・再利用のための正常完了観測は残ります。
完了側は単調なserialで観測し、内容byte列を含む別の完了snapshotを複製・保存しません。
ソース：[import値](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/resource.rs)、
[canonical状態とsnapshot](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/backends/vulkan/graph/snapshots.rs)、
契約：[履歴](rendergraph.md#履歴と段階的な拡張)。

<a id="capacity"></a>

## 13. 容量とbackpressure：拡張は明示準備で行う

```mermaid
{{#include rendergraph-diagrams/capacity.mmd}}
```

Graph slot、metadata、解析領域、native実行枠、物理資源の作成条件、CPU結果の保持枠は別の容量です。
通常frameは準備済み領域を使い、不足を型付きエラーで返します。
slot不足ならconsumerの返却、metadataやworking set不足なら明示準備・変更を利用側が選びます。
通常frameから暗黙にheap拡張、native増設、GPU待機へfallbackしません。

この規約は必要な同期・安全性・容量検査・native失敗処理を削除する理由にはなりません。
既存のゼロcounterは[検証済みworkloadと観測窓](../verification/current.md#通常フレームのメモリ規約と実装)に限る証拠であり、
別負荷の速度やdriver内部の確保についての保証ではありません。
ソース：[GraphCapacity](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/storage.rs)、
契約：[通常フレームのメモリ規約](implementation-policy.md#steady-state-memory)。
