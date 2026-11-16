# RenderGraphを読む

RenderGraphは、**何を読み、何を書き、何を結果として残すか**を宣言すると、
必要な処理とその順序を求め、GPUへ安全に実行するための計画へつなぐ仕組みです。
利用側の描画意図、CPU上の依存解析、backend固有の同期・資源管理を分けて扱います。

この案内は実装確認点[`adb51d1`](https://github.com/tsukushibito/renderer/tree/adb51d17e3c1f43b380163a444f750ee9ff9edf5)の現在の動作を説明します。
詳細な契約は[RenderGraph設計](rendergraph.md)、実行した確認の範囲は[現在の検証状況](../verification/current.md)を参照してください。
後続のB1計画を、実装済みの機能としては扱いません。
型の所有・compile・native実行等を詳しく追うには[設計図集](rendergraph-diagrams.md)を参照してください。

## 1. 宣言からGPU実行へ

最初に全体の境界を押さえます。共通GraphはウィンドウやGPUなしで構築・compileできます。
`compile`が作るのは、backendから独立した不変の`CompiledGraph`です。
VulkanやD3D12のruntimeが、その中の生き残った処理をnative命令へ変換します。

```mermaid
{{#include rendergraph-diagrams/architecture.mmd}}
```

| 境界 | 決めること | 主な入力・出力 |
|---|---|---|
| 利用側・Engine | 描画するもの、パスの構成、残す結果 | meshやframe入力 → Graph宣言 |
| 共通Graph | 内容のproducer、必要なpass、実行順、有効な範囲 | 宣言 → `CompiledGraph` |
| backend runtime | 実資源、stage/access/layout、記録・投入・退役 | `CompiledGraph` → native処理 |
| 結果の利用側 | CPU結果をいつ消費・返却するか | 正常完了したreadback → 準備済み出力領域 |

GraphからEngineの型には依存しません。Engineは不変mesh入力をGraphへ渡し、
Graphのprivateな保証境界を通します。任意の生CommandBufferを受け取るAPIも現在はありません。
操作と宣言した用途・範囲を対応させることで、解析した計画と実際の処理を結び付けます。
現行Engine層はassetとframe入力に加え、標準opaqueや出力のGraph構築も担当します。
採用した[三層の公開API方針](implementation-policy.md#public-api-layers)では、その構成を公開標準パスと標準グラフへ分離します。
ユーザーは標準グラフの全体利用、一部の交換、公開部品を使う独自Graphを選べるようにします。これは今後の移行方針です。

## 2. 小さなGraphを作ってみる

次の例は現在の公開APIだけで構築・compileできる完全なCPUプログラムです。
GPU初期化もbackend featureも必要ありません。buffer作成はこの段階では論理宣言です。

```rust
use renderer::graph::{BufferDesc, GraphCapacity, GraphError, GraphPool};

fn main() -> Result<(), GraphError> {
    // 準備境界。通常フレームの間、poolを保持する。
    let mut pool = GraphPool::new(GraphCapacity::default(), 1)?;
    let mut graph = pool.begin()?;

    let data = graph.buffer("data", BufferDesc { size: 4 })?;
    let scratch = graph.buffer("scratch", BufferDesc { size: 4 })?;

    graph.upload_buffer("old upload", data, 0, &[1, 2, 3, 4])?;
    graph.copy_buffer("unused copy", data, 0, scratch, 0, 4)?;
    graph.upload_buffer("replacement", data, 0, &[5, 6, 7, 8])?;
    let _readback = graph.readback_buffer("readback", data, 0, 4)?;

    let plan = graph.compile()?;
    assert_eq!(plan.pass_count(), 2);
    assert!(plan.ordered_pass_names().eq(["replacement", "readback"]));

    // 全consumerがplanを返すと、このslotを再取得できる。
    drop(plan);
    let next_frame = pool.begin()?;
    drop(next_frame);
    Ok(())
}
```

このプログラムは実行計画を確認します。GPUへの投入やCPU byte列のreadbackは行いません。
native実行では、正常完了後に結果の`readback_into`へ利用側の出力領域を渡します。
例の`GraphCapacity::default()`は説明用です。製品では負荷と同時保持数に合わせて容量を準備します。

## 3. 「必要な内容」と「安全な順序」を分ける

上の例では、最終readbackが読むのは`replacement`の内容です。
`old upload`の内容は`unused copy`だけが使い、そのcopy先は結果として観測されません。
したがって、最初の二つのpassは除去できます。これが不要パス除去、cullingです。

```mermaid
{{#include rendergraph-diagrams/producer-culling.mmd}}
```

ここには別の二種類の依存があります。

| 依存 | 答える問い | 上の例 |
|---|---|---|
| 内容の依存・dataflow | そのreadは、どのwriteが作った内容を使うか | `replacement` → `readback` |
| 実行の順序制約・hazard | 同じ範囲の利用を、どの順序で実行するか | RAW・WAR・WAWと明示依存 |

RAWはwrite後のread、WARはread後のwrite、WAWはwrite後のwriteです。
除去前には`unused copy`から`replacement`へのWARもありますが、
それだけを理由に不要なcopyを生かすことはしません。必要なproducerを辿ってpassを選び、
残ったpassだけでhazardを組み直します。

内容は**宣言順**で決まります。readは、その位置までに確立した内容を読みます。
`depends_on(pass, dependency)`は順序を制約し、dependencyを意味的な前提として残しますが、
後から宣言したwriterをreadのproducerに変える操作ではありません。
一つのpass内でも、全readを検査してからwriteを反映します。
`read_write`という宣言だけで、未初期化の内容を読めるようにはなりません。

compileは概ね次の処理を行います。

1. importの有効範囲を初期内容として、宣言順にread・writeとproducerを解析する。
2. export、readback、副作用、import資源へのwriteから必要なproducer・明示依存を辿る。
3. liveなpassのRAW・WAR・WAWと明示依存を求め、循環を拒否して実行順を作る。
4. 必要ならliveな実行順でwriteを再計算し、最終的に有効な内容を確定する。

未初期化readはculling前に検出します。結果に使わないpassでも不正なreadはエラーです。
一方、除去される部分だけの循環を追加で診断するかは開発用検証の設定によります。
処理の根拠は[共通解析](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/analysis.rs)にあります。

## 4. 内容・GPU状態・完了は別の情報

「画像を持っている」「正しいlayoutである」「GPUが終わった」はそれぞれ違う条件です。
同じように、Graph上の順序が決まっただけではVulkanのmemory dependencyは成立しません。

| 情報 | 意味 | それだけでは保証しないこと |
|---|---|---|
| 内容の有効性 | 読む範囲に定義済みの内容がある | GPU完了、画像layout |
| native予定状態 | stage/access/layoutと必要な同期 | CPUから安全に読めること |
| 実際の正常完了 | Fence等でGPU処理の成功を確認した | 未消費CPU結果の返却、全consumerの終了 |

backendは生き残った実操作からstage/access/layoutを導きます。
Vulkanのbuffer barrierは現在、全bufferを対象とする保守的な粒度です。
論理Graphがbyte範囲を細かく解析することと、native barrierの粒度は一致する必要がありません。
readbackのHOST_READ依存や、非coherent memoryのinvalidateも実行側の責務です。
詳細は[Vulkanの同期方針](../guides/vulkan-best-practices.md#synchronization)を参照してください。

## 5. 資源の範囲とAttachment

bufferは`0..16`のような半開byte範囲で扱い、範囲ごとに異なるproducerを持てます。
論理画像はmip数・layer数を持ち、`image_view(image, mip, layer)`で一つのsubresourceを選びます。
Viewは元の画像を参照する不変値で、独立した資源として重複登録しません。
画像のaspectはformatからColorまたはDepthへ導きます。

`attachment_pass`は描画命令を持たないattachment操作です。
選んだmip/layerの全域を対象に、color配列と任意のdepthを指定します。
選択したtargetのextentは一致させ、同じsubresourceへの重複writeは拒否します。

| 指定 | 内容への効果 |
|---|---|
| `LoadOp::Load` | 既存の有効内容を要求する |
| `LoadOp::Clear(value)` | 選択subresourceの全域を初期化する |
| `LoadOp::Discard` | 以前の内容を捨てる。部分mesh描画だけで全域確立とはみなさない |
| `StoreOp::Store` | 操作後の有効内容を引き継ぐ |
| `StoreOp::Discard` | 後続が利用する内容を無効にする |

exportはbufferまたは画像の**全域**に有効内容を要求します。
一つのmipだけをclearしても、複数mipの画像全体をexportできるとは限りません。
`image_desc`で寸法やformatを取得できることも、内容の初期化や完了の証明にはなりません。

### CPUで計画できる範囲とnative実行

共有`CompiledGraph`を作れることと、選んだbackendが実行できることは区別します。
liveな未対応操作はruntimeが明示エラーにし、culledな枝だけの資源は取得しません。

| 機能 | 共通CPU計画 | Vulkan実行 | D3D12実行 |
|---|---|---|---|
| buffer upload/copy/readback | 対応 | 対応 | transient bufferで対応 |
| 画像upload/readback | RGBA8/RGBA16F upload、これらとD32 readback | 各1 subresource | transient画像・各1 subresource |
| 画像copy | 同extent/formatのRGBA8・RGBA16F | 各1 subresource | 同じ契約のtransient画像で対応 |
| 固定Compute・composite | 対応 | 限定した管理下操作に対応 | 未対応 |
| 管理下owned Raster | mesh/textured meshのRGBA8/RGBA16F、depth-only、複数draw | 最大一色＋任意depth、各1 subresource、指定Load/Store・bindingを保持 | 同じ契約のtransient資源で対応 |
| `Graph::dispatch` | 管理下Programとbuffer・sampled/storage画像のbinding、固定または画像extent由来dispatch | 明示準備したpipeline/descriptorで対応 | 同じ契約のtransient資源で対応 |
| `attachment_pass` | color-only・depth-only・MRT、単一mip/layer View | 最大一色＋任意depth、各1 subresource | 同じnative形状で対応 |
| 複数mip/layerの画像 | subresource単位に解析 | native画像は各1のみ | native画像は各1のみ |
| import/export・履歴 | 宣言・解析に対応 | canonical状態と連携 | 未対応 |
| 複数queue・物理aliasing | 一般実装なし | 単一Graphics＋Compute queue、aliasingなし | 単一DIRECT queue、aliasingなし |

`Graph::dispatch`は`graph::Binding`配列を受け取り、Programのmanifest順に
Buffer・ImageView・Samplerを所有します。callerがread/writeやshader stageを重ねて指定するAPIではありません。
完全なdescriptor範囲と実際のshader footprintは分けます。既存の安全な入口には次の4 assetがあります。

| `ManagedProgram` | 入力の読み取り | 出力の確立範囲 |
|---|---|---|
| `uniform_readback()` | Uniform先頭16 byte | Storage先頭16 byte |
| `storage_readback()` | Storage先頭16 byte | Storage先頭16 byte |
| `sampled_readback()` | 選択mip/layer全域を保守的にread | Storage先頭16 byte |
| `sampled_float_readback()` | 選択mip/layer全域を保守的にread | Storage先頭64 byte |

全てworkgroupsは`[1,1,1]`です。descriptorの余分なbyteを初期化したことにはしません。
Samplerはnearest/clampの不変設定を保持し、別の内容資源やread/write accessを作りません。
これらに加え、利用側のレビュー済みCompute descriptorを明示したunsafe境界で取り込めます。
型付きstorage画像の全域writeは対象extentとdispatchの契約に結び付け、両backendのowned命令へ接続します。

`Graph::raster(name, &program, draws, attachments)`は、借用draw sliceと各drawのbindingをcaptureします。
`ManagedRasterProgram::mesh()`はbindingなし、`textured_mesh()`はCPU計画でUniform・sampled View・Samplerを扱います。
後者はVERTEX段のUniform16 byte、FRAGMENT段のsampled Viewとnearest/clampを要求します。
両者とも頂点stride28、inline行列＋tint80 byte、1 RGBA8＋任意depthというCPU契約です。
RGBA16F出力とdepth-onlyのProgramもあり、draw別bindingは両APIの準備済みPSO/descriptorから実行します。
空draw sliceはattachment-onlyへ正規化するため、その場合のnative形は表の`attachment_pass`に従います。

用途と`ShaderStages`は信頼済みProgram manifestから導きます。
Vertex/Indexのfetch用途とAttachment用途はshader stageと独立に扱います。
ProgramはCPU契約でありnative PSOではありません。未信頼shaderのsandboxや一般native graphicsの対応とは区別します。
所有とstage導出の図は[設計図集](rendergraph-diagrams.md#binding-stages)を参照してください。

## 6. 不変planと、再利用してよい時点

`GraphPool`はpass、resource、access、名前、upload本文、binding、attachment、
依存edge、区間解析等の領域を準備します。`begin`は空いているslotを借ります。
`Graph::compile(self)`は同じ領域を凍結するため、入力全体を別planへコピーしません。
GraphはCloneできず、完成planのCloneは既存slotへのArc pinを増やします。

```mermaid
{{#include rendergraph-diagrams/lifetimes.mmd}}
```

呼出元のupload byte列は構築時に予約済みblobへcaptureします。
bindingやattachmentの短命sliceも、必要な値をplan側に所有します。
入力の変数を変更・破棄しても完成planは変わりません。
builderの途中でエラーになった場合は、追加途中のaccess・payload・名前等を戻します。
失敗したpassの半分だけがGraphへ残ることを防ぎます。

slotを再利用できるのは、planと、それを保持するnative・CPU consumerが全て返却した後です。
`Arc::get_mut`で排他性を確立してからresetし、新しいGraph identityを与えます。
古いframeのhandleは、同じslotを再利用しても新しいGraphでは使えません。
容量を変えるときも旧poolのconsumerを確認し、通常frameの返却で最後のbackingを解放しないようにします。

GPU正常完了後、native実行枠とupload等は先に回収できる場合があります。
未読readbackはCPU結果側に残り、`readback_into`で消費してからそのstaging領域を返します。
未読結果が常にplanをpinするとは限りません。Graph slotは全plan pinの返却、
CPU結果の領域は消費・返却という別の条件で再利用します。
「実行枠が空いた」ことと「全ての資源・結果を再利用できる」ことを同一視しません。
詳細は[slotの所有契約](rendergraph.md#prepared-graph-storage)にあります。

## 7. import/exportとフレームをまたぐ履歴

Vulkanのexport snapshotは、受理済み処理の予定状態を次のGraphへ渡す入口です。
同じqueueにproducer Graph A、consumer Graph Bの順で投入するとき、
BはAのCPU完了を待たずにimportでき、runtimeが必要なnative依存を記録します。
CPU readbackや物理領域の再利用には、別途実際の正常完了が必要です。

snapshotにはruntimeのowner、資源の世代、内容revisionがあります。
別runtime、削除済み資源、内容更新前のsnapshotは拒否します。
readだけの利用や画像layoutだけの変更では内容revisionを進めません。
write・clear・discardで内容が変わった後は、`refresh_buffer`または`refresh_image`で
freshなsnapshotを取得し、Graphを再構築・compileします。投入時にもcanonical状態と照合します。

export資源の登録解除は明示`delete`で行います。CPUハンドルのDropだけでは解除しません。
登録解除後もGPU使用中ならnative所有を維持します。
履歴の初回、reset、extent変更では内容を初期化し直します。
二枚交互に使う番号だけでは、競合の排除や完了の証明にはなりません。
詳細は[GPU状態と所有](rendergraph.md#gpu状態と所有)と[履歴](rendergraph.md#履歴と段階的な拡張)を参照してください。

## 8. 容量不足と失敗をどう扱うか

準備する容量はdraw数だけではありません。変動するpass構成に必要なmetadata、
解析edge・区間・reader履歴、同時保持するplanや未消費結果も含めます。
通常の容量不足では暗黙に増設・heap拡張・GPU待機をせず、型付きエラーを返します。

| 失敗 | 利用側が調べる情報・次の判断 |
|---|---|
| Graph容量・slot不足 | `GraphErrorKind::CapacityExhausted`、`capacity()`のkind/limit/required |
| owner・範囲・未初期化read等 | `kind()`、pass/resource、名前、問題のbyte範囲またはsubresource |
| 循環 | `cycle_passes()`の閉じた有向経路 |
| backend未対応・古いsnapshot | runtimeの構造化エラーに従い、入力や構成を変更 |
| timeout | 完了として扱わず、所有を保持したまま再観測 |
| native失敗・Device loss | GPU完了を推定せず、backendの停止・保持契約に従う |

slot不足なら既存consumerの返却を進め、metadata不足なら明示準備境界で容量変更を選べます。
native OOMと設定済みpoolの空き不足は別の失敗です。
正常完了を証明できない処理から、有効な結果を返したり資源を早期再利用したりしません。

## 9. 製品処理・開発検証・診断を分ける

保証はGraph構築など必要な情報が揃う境界で確立し、不変planと所有で引き継ぎます。
保証済み入力を繰り返し全量検査する処理と、実行に必要な解析・同期は区別します。

| 選択 | 現在の意味 |
|---|---|
| backend feature | Vulkan、Windows D3D12等の実行機能を選ぶ |
| `development-validation` | 保証済み宣言の独立した再点検やdead部分の循環診断等を追加 |
| `graph-diagnostics` | 追加の依存・寿命情報、`dump()`、除去pass名等の観測を追加 |
| native検証・backend診断 | LayerやDebug Layer等の要求、メッセージ・label等を各backendの設定で選ぶ |

`release`という名前だけで検証が消えるわけではありません。
製品にもproducer解析、未初期化拒否、culling、live hazard、容量・所有、
native同期・完了・失敗処理を残します。診断文字列はGPU資源の所有や完了tokenではありません。
詳細な選択と実行コマンドは[検証方針](../guides/validation-policy.md)と[開発手順](../guides/development.md)に従います。

## 10. メモリの設計目標と確認できた範囲

通常frameでは準備済み容量を再利用し、Rendererの汎用CPU heap確保・再確保・解放と、
Renderer発行のnative資源・memory作成・破棄をゼロにする方針です。
構築・compileからprepare、record、submit、poll/retire、present、CPU結果の消費・返却までを対象にします。
固定planだけの反復に限定せず、容量内で入力やpass構成が変わる場合も含みます。

既存の[検証記録](../verification/current.md#通常フレームのメモリ規約と実装)では、
製品releaseのCPU24frame、D3D12/Vulkan buffer各48frame、Vulkan Engine48frame、
Vulkan WSI18source/36present等の指定窓で、対象の確保・解放counterゼロを確認しています。
attachment/subresourceのCPU計画とowned Rasterも、それぞれ24変動frameの別窓で確認されています。
Windows x64、RTX 3060等の条件、準備容量、観測範囲と未実行項目は同記録を正とします。

予約容量による常駐量、区間木の更新、provenance配列操作等の費用は残ります。
counterゼロからCPU/GPU時間の改善や、全負荷・別GPU・未観測driver内部の確保ゼロを推定しません。
CPUだけの試験はnative dispatch対応やnative allocationゼロの証拠にもなりません。
[通常フレームのメモリ規約](implementation-policy.md#steady-state-memory)が目標と証拠の境界を定めています。

## 11. 実装を読む入口と後続設計

| 確かめたいこと | 固定した実装への入口 |
|---|---|
| facade・Graph identity・明示依存 | [`graph.rs`](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph.rs) |
| 準備済みslotと再利用 | [`graph/storage.rs`](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/storage.rs) |
| builderの検査・途中失敗の巻き戻し | [`graph/builder.rs`](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/builder.rs) |
| compileと不変plan | [`graph/compile.rs`](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/compile.rs) |
| owned RasterとProgram契約 | [`program/raster.rs`](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/program/raster.rs) |
| shader stageとbindingの契約 | [`program.rs`](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/program.rs) |
| owned bindingとshader footprint | [`graph/dispatch.rs`](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/dispatch.rs) |
| attachmentの所有と限定lowering形 | [`graph/attachments.rs`](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/src/graph/attachments.rs) |
| 公開APIを使うCPU回帰 | [`tests/graph_api.rs`](https://github.com/tsukushibito/renderer/blob/adb51d17e3c1f43b380163a444f750ee9ff9edf5/crates/renderer/tests/graph_api.rs) |

B1では、実装済みの管理下owned命令とbinding/attachment由来のアクセスを出発点として、
利用側の独自Program/PSO、一般binding、Raster・Computeの二API実行を段階的に実証します。
現在の限定操作を互換性のために固定する方針ではありません。
公開構成への移行は[作業順序](implementation-policy.md#public-api-migration)に従い、Compute/copyの受入後、一般import・寿命管理の前に進めます。
標準グラフの定義を再利用してもframeごとの描画対象は変更できます。基本は入力から各frameのGraphを構築・compileし、
不変のCompiledGraphへdraw列を後から差し込む機能の実装とは区別します。
一般shader、native MRT/mip/layer、Bistro描画や複数queue・物理aliasingの対応状況は、
[公開API比較](implementation-policy.md#public-api-comparison)と[Bistro計画](implementation-policy.md#bistro-roadmap)で確認してください。
