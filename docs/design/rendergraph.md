# RenderGraphの契約

全体像と具体例は[図で読むRenderGraph](rendergraph-overview.md)、
検索・章ナビゲーション付きHTMLの生成は[文書ガイド](../guides/html-docs.md)を参照する。

Graphの宣言・解析はGPUとウィンドウに依存せず、通常のCPU構成で利用する。
Vulkan実行は`vulkan` featureで追加する。Graphの公開型はash・VMAに依存しない。
Windowsの`d3d12` featureは同じ`CompiledGraph`のbuffer upload・copy・readbackと、
画像upload/copy/readback・owned RasterとCompute dispatchを実行する。
Graphの宣言・解析と、各backendが実行できる操作の範囲は区別する。
全体方針は[実装方針](implementation-policy.md)、native資源と完了管理は
[Vulkanの設計](vulkan.md)と[D3D12の設計](implementation-policy.md#d3d12-runtime)、
実行済みの確認は[検証状況](../verification/current.md)を参照する。

検証処理の配置・製品構成からの除去には[製品版と開発用の検証方針](../guides/validation-policy.md)を適用する。
以下は現在のAPI・動作の契約である。backend選択、追加検証、診断保持は独立に選ぶ。

次の開発は[実装方針のBistro計画](implementation-policy.md#bistro-roadmap)に従う。
B0で実シーンの要求・公開API利用例と[基盤の採用・移行範囲](implementation-policy.md#foundation-selection)を設計した。
B1で構築時owned命令とbinding/attachment由来アクセスを一般化し、nativeの低レベル所有を再利用して二APIへ接続した。
B2の利用側cook・PBRもこの経路で[実GPU受入と測定](../verification/current.md#bistro-pbr)を行った。
[公開APIの採用方針](implementation-policy.md#public-api-layers)は、汎用Graph API・公開標準パス・それらで構成する標準グラフの三層とする。
Engineの固定構成からの移行順序は[公開API変更計画](implementation-policy.md#public-api-migration)に従い、以下の現行契約と区別する。
現行の固定操作、内部表現、構築順による意味論、runtimeの再利用は維持義務ではない。
以下の現行契約は、変更を実装・検証するまで有効であり、計画だけで対応済みとは扱わない。

## ブラウザの WebGPU ランタイム

`wasm32-unknown-unknown` の `webgpu` feature では、ホストが取得した WebGPU デバイスとキューを
`Runtime::from_webgpu_device` に渡す。初期化は非同期で、シェーダーや資源を準備した際の
WebGPU の検証・内部エラーとメモリ不足を呼び出し元へ返す。上位パイプラインはネイティブ版と
同じ `CompiledGraph` を submit し、同じエクスポートと所有者を使って後続フレームに入力する。
表示先がエクスポートされた画像を参照する場合、その表示コマンドを同じキューに投入するまで
画像の所有者を保持する。

ブラウザでは GPU と CPU 読み戻しの完了通知がイベントループを通る。通常フレームでは
`poll_frame()` または `wait_for_frame(id, Duration::ZERO)` で到着済みの結果を取り出す。
素材準備やキャプチャ時は `wait_for_webgpu_frame(id).await` で指定した投入の完了を待てる。
この future を取り消した後もランタイムは受理済みの処理を保持し、後から同じ ID を観測できる。
同期 API に正の待機時間を指定すると `Unsupported` を返す。確保済みのフレーム枠や
読み戻し領域の再利用には、実際に GPU 完了を観測し、対応する CPU 側の参照を返す必要がある。

## ソースの責務と依存方向

`graph.rs`は公開型のfacade、Graph所有状態、identityとcompile入口を持つ。
子moduleの`resource`は論理資源・import・範囲、`pass`は宣言とowned命令、
`error`はprivate contextを持つ構造化エラー、`builder`は検査後のtransactionalな追加を担当する。
`storage`は準備済みslotと借用範囲、`ranges`はimport初期化範囲の不変snapshotを所有する。
`accesses`/`analysis`は予約済み作業領域で解析し、`compile`が構築領域を不変planへ凍結する。
`provenance`は両native backendが使うprivateなindex内容追跡と不変meshの要約を保持する。
upload/copyによる保証の確立・失効、未知の穴の拒否、容量とprefix最大値の再利用を一つの実装へまとめる。
既存builder回帰群は`graph/tests.rs`へまとめる。
公開型はfacadeから直接reexportし、並行する互換APIを作らない。

画像形式はGraphとProgramで共通の`sashiko_renderer_core::ImageFormat`を使う。
`Rgba8Unorm`、`Rgba8Srgb`、`Rgba16Float`、`Depth32Float`の意味を一つの型で定め、
論理資源とshaderのformat契約を別enum間の変換で対応づけない。
nativeのresource／sampled view／depth viewの形式は用途ごとに導出し、
共通format型の採用を未対応の用途やformatを許可する理由にはしない。

Graphはメッシュ台帳に依存しない。`GeometryData::upload_graph_slice`は不変なstreamと
既存のindex要約を選び、Graphの狭いprivate builderがowner/全stream長/alignment/範囲を検査して追加する。
engine側からGraphのpassを直接変更せず、任意のpublic byte uploadからmesh provenanceを作らない。
元のArc、slice位置とappend失敗時の状態を維持し、chunkごとのpayload複製や追加scanを入れない。

一般の頂点形式には`geometry::GeometryData::new(vertex_stride, vertices, indices)`を使える。
非ゼロの4-byte整列stride、stream長、非空triangle list、u32 countと全indexの頂点範囲を生成時に保証する。
頂点fieldの意味・有限値・paddingはopaqueであり、Bistroではcook/loaderが48-byte形式として保証する。
`upload_vertices`／`upload_indices`はprivateなstreamと全index streamの最大値を既存のmesh upload経路へ引き継ぐ。
native provenanceはbyte長・既知の全stream範囲・最大indexを保持し、検査用CPU index byte列やproducer Graphを保持しない。
この証明を任意prefix drawへ転用せず、実Programのstride・頂点数・Device上限はnative適合境界で確認する。
任意のwrite/copyは既存のprovenance規則で失効・再確立する。
同じ長さの全index streamをtyped uploadで置き換える場合は、新しいstreamの最大index要約へ置換する。
setupのGraphPool/planはupload streamのArcを保持し得るため、GPU完了後の明示した準備・保守境界で
GeometryDataとidle upload poolを解放する。次の通常frameのresetへ最後のCPU解放を持ち越さない。

## 対応範囲と処理の境界

Vulkanの経路は、単一Graphics＋Computeキュー、物理aliasingなしを基準とする。
両backendのnative画像は2D・1 layerとする。実行できる画像の形状と用途は次のとおり。

| 用途 | Vulkan／D3D12の共通範囲 |
|---|---|
| Raster・attachment操作 | RGBA8_UNORM／RGBA16Fのcolor attachmentを最大8枚、任意のD32 depth。各画像1 mipでextentとsample数を揃える |
| MSAA | 実Deviceが対応する形式の4× color/depth attachment。colorは同形式・同extentの1×画像へresolveできる |
| sampled入力・upload | 1×画像でsampled Viewは初期化済みの連続mip範囲。uploadは対応するcolor形式の全chain・選択したmip・矩形 |
| storage・copy・readback | 1×。storage/readbackは1 mip。矩形copyは同形式の別color画像の指定mip間。readbackは対応するcolor形式とD32 |

4×画像はattachment用途に限定する。形式ごとのshaderの型・用途と、実Deviceの上限も適用する。
論理Graphは、以下のより広いmip/layer単位のCPU計画も扱う。
bufferは半開byte範囲を指定する。
論理ハンドルと物理資源の識別、内容の有効性と画像layout、
予定されたGPU状態と実際の完了を別々に管理する。

通常のパスは、copy、管理下Compute、attachment操作、順序付きRasterを宣言する。
同じDeviceに属するエンジン所有画像は、下記の登録を経て通常の `ImportImage` として使う。
利用側のCompute shaderは、静的descriptorまたはロードしたバイナリとABIを明示したunsafe境界で取り込む。
これにより、実行する操作と宣言した用途・範囲・bindingを対応づける。
汎用RHIや未信頼shaderのサンドボックスを提供する契約ではない。
依存するCompute処理は別ノードとし、パス間の同期を実行基盤が記録する。

| 実行基盤 | 現在の範囲 |
|---|---|
| Vulkan | bufferと画像のupload・copy・readback、owned Compute・Rasterとattachment操作、canonical import／export・履歴と上限付き再利用 |
| D3D12 | bufferと画像のupload・copy・readback、owned Compute・Rasterとattachment操作、canonical import／export・履歴と上限付き再利用。単一DIRECT queue、committed resource、物理aliasingなし |

両APIともnative未対応の操作や準備済み容量の不足をprepareで明示エラーにする。
共有Graphを構築できることだけで、すべてのbackendがその操作を実行できるとは扱わない。

### エンジン所有テクスチャをGraphへ渡す

GPU を初期化済みのエンジンは、既存のデバイスとキューから Runtime を作成できる。
Vulkan は `Runtime::from_vulkan_device` に実際に有効化した機能とキューのロックを渡し、
D3D12 は `Runtime::from_d3d12_queue` に DIRECT キューを渡す。どちらも Runtime の作成前に確保した画像を登録できる。
[Vulkan のデバイス採用と寿命](vulkan.md#host-textures)、[D3D12 のキューと診断設定](implementation-policy.md#d3d12-runtime)

1. `Runtime::as_vulkan_mut` または `as_d3d12_mut` で選択中のbackendを取得する。`external_device()` はnative Deviceを保持するownerを返す。エンジンはこのDevice上で作成したテクスチャを渡す。入力テクスチャと最終描画先で同じ手順を使える。
2. ホスト側の先行アクセスを完了させ、キューと画像状態を合わせて `unsafe register_external_image` を呼ぶ。成功から返却までは、ホストの別queue・CPU・aliasを含めてRendererに排他的アクセスを渡す。登録時に予算を消費する。複数frameで利用する画像は登録を維持する。
3. 戻り値を `Graph::import_image` に渡す。次のGraphで最新の書き込みを使うときは同じkeyで `Runtime::refresh_image` を呼ぶ。登録時の確認やview作成は通常のフレーム投入経路に持ち込まない。
4. `release_external_image(key)` は記録・GPU・表示のleaseが残る間 `Ok(None)` を返す。未投入の記録を手放し、投入済みframeをpollまたはwaitで退役させて再試行する。`Ok(Some(..))` でホストのアクセスを再開でき、古いkey・snapshotは失効する。GPU完了を確認できないエラーでは排他的アクセスを返却したものと扱わない。

Vulkanではhostのallocation owner、layout、初期化の保証を渡し、返却されたlayoutを次のbarrierに使う。[Vulkan側の所有権と同期](vulkan.md#host-textures)

D3D12では `windows_core::IUnknown` を受け取り、実際の `ID3D12Resource`、`GetDevice` のCOM identity、descriptorとheapを登録時に確認する。単一nodeのDEFAULT heapのcommitted 2D textureを全subresourceがCOMMONの状態で渡す。返却状態もCOMMONで、戻り値のboolが全mipの初期化を表す。1×のRGBA8 UNORM・RGBA16FはRTとUAVの両方、4×はRT、D32はR32_TYPELESSとDEPTH_STENCIL、RGBA8 sRGBは追加flagなしを使う。Graph用の画像slotをdescriptorごとに予約するか、登録後に一度 `prepare_working_set` を呼ぶ。hostはResourceをresidentに保ち、Rendererは独立のCOM参照を保持する。[ID3D12DeviceChild::GetDevice](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12devicechild-getdevice)

`program::ManagedProgram`はComputeの不変ABIとアクセス範囲を定義する。
`Graph::dispatch`が各bindingと論理資源を所有し、両runtimeが明示準備したpipelineとdescriptorへ具体化する。
sampled image ViewとD32のdepth attachmentは画像の用途とformatで区別し、aspectはformatから導出する。
StorageReadとStorageWriteはStorage資源へのアクセス用途であり、資源作成やbindingだけで内容を初期化しない。
[管理下Programの契約](vulkan.md#managed-uniform)を参照する。

<a id="owned-shader-bindings"></a>

### Program由来bindingとshader stageのowned計画

`Graph::dispatch(name, &program, &bindings)`は不変の管理下Compute Programとbindingから実行範囲を決める。
`Fixed`はProgramの固定workgroup数を使う。`Image2d`は出力Viewのmip extentをlocal sizeで切り上げ除算する。
毎回の入力から可変な`BufferGrid2d`は`Graph::dispatch_buffer_grid(name, &program, &bindings, [x, y])`で指定する。
`graph::Binding`はgroup/bindingと
`BindingResource::{Buffer { buffer, offset, size }, ImageView, Sampler}`をまとめる。
Rasterは`Graph::raster_bindings(&program, &bindings)`で同じ型のsliceを一度検査・所有し、
各drawが不変な`RasterBindings` handleを共有する。用途・shader stage・実アクセス範囲をProgramから導く。
利用側に別のread/write一覧を要求せず、旧buffer専用入口は残さない。

命令は管理下Programの不変な版ID、dispatch数、manifest順のbindingを保持する。Rasterでは
drawごとの定数ワード数も記録する。準備済みarenaへコピーしたbuffer descriptor・subresource View・
sampler設定により、呼出元のProgram変数・binding/draw sliceが変更・破棄されてもplanは変わらない。
実行時はWorkingSetが所有する同じ版IDのネイティブパイプラインとABIを参照する。
Graphの返却や構築失敗ではProgramを解放する処理が発生しない。版が準備済みでないplanは投入前に拒否する。
[外部シェーダーの取込契約](implementation-policy.md#external-shader-assets)を参照する。
同じRaster内のdrawごとに異なるmaterialのhandleを指定できる。命令は論理Graph資源を参照する。

全slotの過不足・重複・種別・owner、非ゼロのbuffer範囲・ABI最小size・論理境界を構築時に確認する。
解析にはABI最小sizeから推定した量ではなく、shaderと同時にレビューするfootprintを使う。
uniform/storage readbackは先頭16 byteだけを読み書きし、sampled readbackの出力は16 byte、
sampled float readbackは64 byteだけを初期化する。大きなdescriptorの未書込tailは有効にならない。
`BufferGrid2d`は指定した書き込みバッファのbinding offsetから、XとYのworkgroup数の積に
1グループ分のバイト数を掛けた範囲を初期化する。Zは常に1とし、構築時にX/Yの非ゼロ条件と上限、
積のoverflow、実際のdescriptor範囲を検査する。取込側は受理される入力全体で、
各workgroupが競合せず、対応する出力を過不足なく書き込むことを保証する。
他のbindingは固定のfootprintを使い、GPUでのdispatch上限・descriptor適合・同期は既存の実行経路に従う。
sampled入力は選択したViewのmip範囲/layer全域を読むものとして扱い、sampler自体は資源内容アクセスを作らない。
LODが特定のmipへ偏ると推測して、選択範囲の未初期化検査やhazardを省略しない。
shaderやdispatch条件を変える際はfootprintと初期化保証も改訂する。

`StorageImageRead(format)`と`StorageImageWrite(format)`は型付きの単一subresource Viewを使う。
Graph構築でformatと対象extent、dispatch数を結び、全域writeのレビュー済み保証を満たす出力だけを
初期化済みとして扱う。読みながら同じsubresourceへ書くfeedbackは別passへ分割する。
新しいeffect名をGraph命令やbackendの列挙へ追加せず、利用側がProgramとbindingを配線する。
未信頼shaderの安全性をreflectionだけから推定しない。unsafeな取込の責務は
[Programの契約](vulkan.md#managed-uniform)を参照する。

`BindingLayout::stages`はVertex/Fragment/Computeを表し、用途と共に実行計画へ保持する。
UniformをStorageReadで代用せず、vertex/index fetchはshader stageとは別の入力用途として扱う。
同passでwriteを含む実範囲が重なるbinding、および全drawのsampled入力とattachmentのfeedbackを拒否する。
read/read重複と同じ画像の異なるmip/layerは区別する。初期化・宣言順producer・RAW/WAR/WAW・
culling・循環は既存の共通解析へ接続する。

`RasterBindings`はGraph identityと正確なProgram identityに結び付く。layoutが同じでも
review済みfootprintが異なり得るため、別Programへ流用しない。handleのprivateな範囲は成功した
作成処理だけが公開し、Graph内のarenaはappend-onlyで、失敗時は未公開の追加分だけを戻す。
この不変条件によりdrawではownerとProgramだけを再照合し、slot・descriptor・View範囲を再走査しない。
poolの次のcheckoutは新Graph identityとなり、旧handleを拒否する。
`RasterDraw::bindings`の`None`はbinding slotのないProgramに限る。

set作成はdescriptorとread footprintのtemplateだけを保存し、その時点ではpass accessを作らない。
各使用passでtemplateを一度追加するため、同じsetを複数passで使っても宣言順producer・cullingを保つ。
信頼済みRaster契約は読み取りbindingだけを許し、shader副作用とdraw間同期を禁止する。
全drawの順序・定数・native descriptor operandは保持し、既存のflat arena上の範囲を共有する。
pass内の重複readは範囲・purpose・shader stage・全access属性が完全一致する場合だけ一件にする。
write、異なる用途・stage、部分的に重なる範囲は統合しない。binding slotに依存する拒否と
pass内overlap検査を終えてから圧縮し、Computeのwrite hazardやimage layoutの拒否を維持する。
Rasterはbindingがあればpass全体を一度検査し、bindingがなければbuffer入力とimage出力が異なるため
attachmentだけを一度検査する。attachment-onlyでも重複Viewを拒否する。

pass内のwriterが4件以下なら、元の全accessとwriter列だけを直接比較し、
競合pairの宣言indexが辞書順で最小のものを返す。自身との比較を除外し、
buffer/imageの半開範囲とresource同一性を従来と同じ条件で判定する。
多数のreaderまでsortする費用を避ける一般的な小集合経路であり、draw数や固定planに依存しない。
5件以上のwriterは従来の全accessの区間索引へ戻し、最悪O(A log A)を保つ。
writerがあれば全access数Aについてscratch容量を確認し、all-readerはscratchを使わない。

`GraphCapacity::bindings`はdispatchのoperandと、各Raster setのoperandで共有する上限である。
同じsetを使うdrawごとの追加消費はない。空setはhandleの値以外のmetadataを保存しない。
templateと使用済みsetの作業領域はbinding上限以内に準備する。exact-read indexはaccess上限をAとして
`(2A + 1) × size_of::<usize>()`のbucketと`A × size_of::<usize>()`の使用bucket列を準備する。
resource上限Rに対し、最初のreadの保持位置・hash昇格markと使用resource列も
`R × (size_of::<FirstRead>() + size_of::<usize>())`（Windows x64では24R byte/slot）予約する。
resourceの最初のreadと、その全属性が完全一致する反復はhashせず処理し、
異なる範囲・用途・stage等が現れたときだけ最初のreadをhash索引へ一度登録する。
以後の異なるreadは既存のhash索引を使い、多数rangeをresourceごとの線形列へ変えない。
保持位置は圧縮中も後続要素に上書きされず、各passの開始とslot resetで使用resource/bucketだけを消す。
代償は準備時のmetadata増加であり、追加容量・通常frameの確保・失敗時の再走査へ移さない。
昇格後も最初のreadとの完全一致比較を先に行い、そのreadの反復をhashへ戻さない。
代案の昇格後hash直行はこの比較を省けるが、最初のreadが反復する場合の費用を戻す。
現在はBistroの実測効果と完全一致readの再利用を選び、単一resourceの多数の異なるrange等で残る
追加定数費用を[前後比較の交換条件](../verification/current.md#rendergraph-hotspot-optimization)へ記録する。
templateは最大`bindings × size_of::<ResourceAccess>()`、setの使用markと
列は最大`bindings × (size_of::<bool>() + size_of::<usize>())`を各Graph slotに準備する。
access容量は圧縮前の構築作業領域も覆う。通常frameでheapを増設せず、容量不足は明示エラーとする。
set作成失敗はoperandとtemplateの追加を戻し、後続pass失敗は既存setを維持してdraw/access/attachment/nameを戻す。
planとnative consumerのpinが全て返るまでslotを再利用しない。

native dispatchは明示準備したProgramのpipelineと容量付きdescriptorを使い、通常frameで作成・増設しない。
descriptor-bound Rasterは、明示準備したProgram・attachment構成のnative PSOとdraw別descriptorへ具体化する。
culledな未対応枝は実行を妨げず、その枝専用のnative資源も取得しない。
Device固有のalignment・usage・descriptor/dispatch上限、native Program/binding所有、
D3D12のresource単位の状態互換性はCPU計画だけでは保証しない。

`examples/graph_composition.rs`は実Graph上で、短命binding/draw列、利用側に隠した中間画像、
二つのsampled Rasterと出力handleを示す。無効時は入力handleをそのまま返せる。
設計比較用`Study/scenario/bloom`の構成・所有パターンを実APIへ接続する例であり、
Bloomアルゴリズム、HDR Graph画像、一般native shader実行の完成を示すものではない。

<a id="prepared-graph-storage"></a>

## 準備済みslotとframe入力の所有

`GraphPool::new(GraphCapacity, slots)`は明示的な準備境界で領域を確保する。
通常の構築は`pool.begin()`でslotを借り、`Graph::compile(self)`が同じ領域を
`CompiledGraph`へ凍結する。旧`Graph::new()`と非消費型compileの互換経路は残さない。
GraphはCloneできず、完成planのCloneは既存slotへのArc参照だけを増やす。
構築途中のDrop、compile失敗、planのDropはslotへの参照を返す。

容量はresource、pass、access、draw、binding、attachment、明示依存、初期化range、名前byte、upload byte、
解析edge、区間、reader nodeを含む。draw数だけでなく、変動するpass構成・入力が
必要とする全metadataも準備した上限内へ収める。上限超過や使用中slot不足は
`GraphErrorKind::CapacityExhausted`で返し、通常経路で拡張・別確保・GPU待機へfallbackしない。
pass/access/draw/payload/名前はflatな予約済み配列と範囲で保持する。
入力を解析用access Vecへ写し直さず、予約済みworkspaceを毎回resetして動的に解析する。
同じplanだけを反復するcacheや、固定pass構成だけの経路ではない。

poolは通常処理の間保持する。`begin(&mut self)`は`Arc::get_mut`で排他性と退役時の同期を
確立できるslotだけを再利用し、新しいGraph identityを与える。旧frameのhandleは再利用先で拒否する。
native命令、provenance、CPU結果等がplanを保持する期間もslot数へ含め、
記録・GPU・CPUの必要なconsumerが終わる前に領域を書き換えない。
`is_idle(&mut self)`はpool置換前の同期付き確認、`leased_slots()`は観測用の件数である。
poolを先に破棄しても残存planは安全に読めるが、通常処理を継続する容量変更では
旧pool ownerを保持し、consumer終了後の明示保守で破棄する。
返却による最後のheap解放だけを通常frameの計測から除外する意味ではない。

`ImportBuffer`の初期化範囲は`ImportRangesPool::new(range_capacity, slots)`が作る
不変tokenへ移行した。`capture(&mut self, &[Range<u64>])`とtokenのClone・返却は
準備済み領域を使う。Graphへのimportはその範囲を自身の予約領域へcaptureし、
runtimeのowner・世代・内容revisionとの照合契約は維持する。
`ImportRangesPool`を変更するときも、旧tokenのconsumerが残るpoolを通常frame中の最終解放へ移さない。

CPU assetとsnapshotの準備は後述の`MeshCapacity`と`SnapshotPool`が担当する。
native実行枠・物理資源・readback結果の準備は各backendの所有契約で別に管理する。
全段階の達成可否は[通常フレームの規約](implementation-policy.md#steady-state-memory)と
[検証状況](../verification/current.md)で確認し、この共通所有契約だけから
native作成・破棄や表示・結果消費を含む全体のゼロ達成を主張しない。

<a id="capacity-planning"></a>

### Graph容量の数え方と不足時の扱い

`GraphCapacity`は一つのslotについての予約量であり、pool全体の量はslot数に応じて増える。
通常frame内で増設する上限や、backendの物理資源数ではない。

| 上限 | 数えるもの |
|---|---|
| `resources` / `passes` | 宣言した論理資源とpass。compileで消える枝も構築時には容量を使う |
| `accesses` | passが持つ資源アクセス。Rasterの重複read圧縮前の作業領域も覆う |
| `draws` | 全Raster passのdrawレコードの合計。間接描画はバッチごとに代表drawを1件保持する |
| `raster_constant_words` | 全Raster passで保持するインライン定数の`u32`ワード数。`Some(n)`は明示上限、`None`は`draws × 32`を上限とする |
| `bindings` | ComputeのoperandとRaster setのoperandの合計。同じsetを使うdraw数ではない |
| `attachments` | 各passのcolorとdepth attachment operandの合計 |
| `dependencies` | `depends_on`で明示した重複しない依存 |
| `initial_ranges` | importからGraphへcaptureする初期化済み範囲の合計 |
| `name_bytes` / `upload_bytes` | UTF-8名と公開uploadからcaptureするpayloadのbyte合計 |
| `edges` | producer/dataflowと実行順序の各arenaへ同じ上限を予約する。二つの合計ではなく、pass数だけからは決まらない |
| `segments` | 内容のproducer／有効性と最後のwriterを表す各区間arena、および最終初期化範囲の上限。部分writeで増え、全域置換で再利用する |
| `readers` | 生存するread-only accessの範囲索引。開発用の全宣言診断ではdead部分のreadも対象 |

`GraphError::capacity()`の`kind`と`limit`から不足した領域を選ぶ。`required`は観測できた最小必要量で、
解析時の不足などでは`None`になり得る。失敗時の値だけで、後続宣言も含めた全体の必要量を保証しない。
`Slots`の不足ならplan/native consumerの返却を進める。配列の上限不足なら、明示した準備・変更境界で
必要な上限を変更し、新しいpoolでGraphを再構築する。`compile(self)`は失敗時もGraphを消費し、
同じ構築途中の値へ戻るAPIではない。旧poolのconsumerが残る場合はownerを保ち、idle後に保守として解放する。

大きめの容量を予約してから想定する可変入力・pass構成を確認する。資源数だけを増やしても
範囲や依存の不足は解消しない。Graphの容量とruntimeのworking set、descriptor、provenance、
結果保持の容量はそれぞれ独立して準備する。

## producer・範囲・Attachment

### 画像subresourceとowned attachment配列

`renderer-7ft.2.2.3`。`ImageDesc`は`mip_levels`と`array_layers`を持ち、
`rgba8`/`rgba8_srgb`/`rgba16_float`/`depth32` constructorは各1を設定する。mip数は最大extentから許される段数まで、
layer数は非ゼロとし、全mipの縮小extentとlayer数のpacked byte合計をchecked演算で検査する。
これはnative allocation sizeやVRAM使用量の見積もりではない。

`Graph::image_view(image, mip, layer)`は同じ画像の単一subresourceを指す不変な値を返す。
owner・範囲を作成境界で確認し、aspectを色形式のColor／D32のDepthから導く。
`ImageView`は画像を別resourceとして登録せず、複数Viewの重なりを元の画像で解析する。
`Graph::sampled_image_view(image, base_mip, mip_level_count)`は単一layerの連続mip範囲を選ぶ。
`ImageView::mip_level_count()`で範囲を確認でき、`image_view`のcountは1である。
storage/attachment View は count1 を要求する。native sampled 実行は単一 layer の連続 mip 範囲を扱う。
論理解析は`layer * mip_levels + mip`の区間へ写像し、producer・未初期化拒否・RAW/WAR/WAW・cullingを共通解析へ接続する。
`declare_analysis_pass`の画像範囲も同じ各subresourceを扱う。エラーでは最初の未初期化subresourceをmip/layer/aspectへ戻す。
`export_image`は全subresourceの有効内容を要求する。単一layerでは `export_image_mips(image, levels)` で公開するmipを指定・累積できる。
`ImportImage::is_mip_initialized` が示す初期化済みmipの範囲だけを次のGraphに引き継ぐ。

`Graph::attachment_pass(name, RasterAttachments)`は描画命令を持たないattachment操作を追加する。
color配列は0個以上、depthは任意だが、全体が空の指定は拒否する。
各`Attachment { view, load, store }`を準備済みarenaへcopyし、呼出元のsliceを変更/破棄してもplanは変わらない。
選択mipのextentとsample数は全targetで一致させる。色/深度の形式、clear値、owner、同一subresourceへの重複writeを構築時に確認する。
depth clearは有限な0〜1とし、subnormal値を拒否する。二APIのnative clearとshaderによる全域clearの間で
FP32のflush差を持ち込まないための保証であり、既存managed D32作成のclear値と同じ境界を使う。
ゼロと`f32::MIN_POSITIVE`以上の通常値は受け付け、この保証をprivateなowned operandで保持する。
同じ画像の非重複layerは別のtargetにできる。Load/Store/Discardは選択subresourceだけに適用し、
loadがDiscardの操作だけで有効な内容を確立したとは扱わない。
`GraphCapacity::attachments`はcolorとdepthを合わせた保持数の上限で、失敗時には途中のarena/access/name追加を戻す。
旧`clear_attachments`はこのAPIへ置換し、互換入口を残さない。

両backendのnative attachment操作とRasterは、デバイスが対応する範囲で最大8枚のカラー（RGBA8またはRGBA16F）と
任意のD32を扱う。カラーなしのD32パスも構築できる。複数layerやmulti-mip画像をattachment/storageへ使う
live計画はpreflightで拒否する。
sampled mip chainのcache identityはmip数も区別し、同じbase extent/formatの1mip画像を誤再利用しない。
D3D12のprepareは既存execution枠の取得後に適合を検査する。未準備容量の不足が先に返る場合があり、
全backendのエラー種別の優先順位を固定する契約ではない。
culledな未対応枝は実行や物理資源取得を妨げない。

画像は `SampleCount::X1` を既定とし、カラーとD32の単一subresourceアタッチメントには `X4` も選べる。
`RasterAttachments::color_resolves` は空、またはカラーと同数のslotを持つ。
resolveするslotではLoadまたはClearで4×画像の全域を確立し、同じextent・formatの1×画像を指定する。
resolve先は独立した書き込みとして依存解析に入り、元の4×画像をStoreで破棄しても後続で使える。
VulkanはDynamic Renderingのカラーresolve、D3D12は描画後の `ResolveSubresource` にloweringする。
4×画像はアタッチメントで使い、sampling・storage・upload・readback・exportには1×画像を使う。
通常フレームの前に、各画像クラスと `RasterPipelineRequest` のProgram／sample数を準備する。

whole-image uploadはRGBA8（UNORM/sRGB）、RGBA16F、RGBA32F、BC1/BC3/BC7（UNORM/sRGB）、BC5（RG UNORM）、BC6H（RGB UFLOAT）を扱う。
`ImageDesc::texture_2d(width, height, format).with_mip_levels(count)` で形式とlevel数を選び、mip 0から順に各levelのデータを連結する。
RGBA画像は隙間のないtexel行、BC画像は4×4ブロックの行を使う。各mipの幅・高さを個別にブロック数へ切り上げ、
BC1は1ブロック8 byte、BC3/BC5/BC6H/BC7は16 byteとする。1×1の末端mipにも1ブロックを渡す。
`ImageFormat::packed_row_bytes`、`packed_rows`、`packed_byte_len` でそれぞれの寸法の転送量を得られる。
Graphは全chainの長さを照合して全levelを初期化し、backendがnativeのpitchとoffsetへ変換する。
`Graph::upload_image_region` と `copy_image_region` は、単一layerのcolor画像を `ImageRegion` のmip・左上座標・寸法で指定する。
mip全域の書き込みはそのmipを初期化する。矩形は周囲の既存画素を保持するため、初期化済みmipを要求し、前のproducerを生存させる。
圧縮形式では左上を圧縮ブロックに揃え、右端・下端の不完全ブロックはmipの端だけに許可する。
転送byte列は各ブロック行を隙間なく並べる。画像間の矩形copyは等しい形式の別画像に同じ寸法で行う。
同じextentとformatの1×・1 mip画像間の全体コピーは `copy_image` で指定できる。
1×・1 mipのreadbackは上記のcolor形式とD32を元のpacked bytesで返す。
RGBA8 sRGB、RGBA32F、BC形式はsampled/transfer用途に使う。sRGBはRGBのみをsampling時にデコードする。
D3D12でmip 0のBC画像の幅または高さが4の倍数でない場合は、準備時に
[UnalignedBlockTexturesSupported](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/ns-d3d12-d3d12_feature_data_d3d12_options8)
を照会する。下位mipの寸法は1 texelまで使える。
エンジンのアセットローダーは `Runtime::upload_texture(pool, name, desc, bytes)` で
`(FrameId, ManagedImage)` を受け取る。所有者をアセットと一緒に保持すると、後続のGraphは
同じRuntimeで `image.snapshot()` を即座にimportできる。GPU完了は通常のフレームと同じ方法で回収する。
複数の画像を同じ投入で扱うときは `Graph::upload_texture_for_export` を繰り返し、返されたハンドルで
同じGraphの描画や読み戻しを宣言できる。`Runtime::submit_with_images(&plan, [base, normal])`
は引数順の所有者を返し、各所有者は独立して解放できる。入力長や宣言容量のエラーでは
失敗した画像の宣言を取り消し、投入失敗では予約した解放枠を全て返す。
linear filteringの実format能力は作成・準備境界で確認する。
`SamplerDescriptor` はU/Vごとにrepeat、clamp-to-edge、mirrored-repeatを選び、拡大・縮小それぞれの
nearest/linearと、mipの不使用・最近傍選択・線形補間を表す。mipを使わない場合は選択Viewの先頭レベルだけを読む。
`from_gltf` は指定値を厳密に変換し、省略したwrapをrepeat、拡大・縮小・mipをlinearへ正規化する。
`NearestClamp`、`LinearRepeat`、`LinearClamp`、`LinearRepeatUClampV` は同じ型の定数である。
利用する値を `Preparation::reserve_sampler` で集約し、`WorkingSet::samplers` としてDeviceの作成・再準備へ渡す。
ロード済みDeviceには `Runtime::prepare_samplers` で追加できる。Device内では同値の要求を共有し、
未準備の値を使うsubmitはエラーを返す。比較とanisotropyはこの2D samplerの対象外とする。
trusted Rasterの`sampling_pairs`は、shaderが使う画像とsamplerの全組み合わせを宣言する。
実samplerがlinearな組の画像だけにlinear-filter能力を要求し、Load/texelFetch専用画像は組を持たない。
Programを差し替える場合はこの保証も再確立する。同じ不変RasterBindingsの能力照合はsubmit内で一度とし、
準備済みの領域で結果を引き継ぐ。Computeの保守的な画像能力条件は別契約として維持する。
画像単位の保守的なnative state/layout管理を維持し、一部の操作を画像全体の初期化として扱わない。

資源は可変で、readは宣言順で直前までに書かれた内容を使う。
bufferの異なるbyte範囲には異なるproducerがあり得る。
一つのパスのreadをすべて検査してからwriteを反映するため、
`read_write`宣言自体では未初期化内容を初期化できない。
forwardの明示依存は実行順序を制約するが、readが読むversionを変更しない。
明示依存の先は意味的な前提として残す。liveな循環は常に拒否し、
`development-validation`ではdeadな部分の循環も診断する。製品構成は除去されるだけの循環を拒否しない。
ハンドルはappend-onlyなGraph内だけで有効で、別Graphでの利用を拒否する。

`Graph::image_desc(ImageHandle)`はownerと画像の型を検査し、`ImageDesc`を返す。
一時画像とimport画像、colorとdepthで同じ問い合わせを使える。
内容の初期化やGPU完了を要求せず、descriptorの取得がそれらを保証するわけでもない。
custom処理は入力handleから形式・寸法を取得して出力を作れるため、Viewの寸法を別に渡す必要はない。

構築・compileの失敗は`GraphError::kind()`で分類する。`GraphErrorKind`は非網羅の列挙型で、
入力不正、無効handle、owner不一致、未対応、import snapshot不一致、未初期化read、循環、
利用側のcustom失敗と準備容量不足を区別する。`pass`／`resource`、名前、`AccessRange`を取得でき、
bufferは問題のbyte範囲、画像はmip・layer・aspectを返す。内部の画像用論理範囲`0..1`は公開しない。
循環は`cycle_passes()`で、先頭を末尾に再掲した有向経路として実際の参加passを返す。
同名passもhandleで区別でき、循環から出るだけの後続passは含めない。
構築中に拒否したpassはまだhandleを持たず、要求名だけをcontextへ残す。
別Graphのhandleから現在のGraphの同じindexを参照して名前を取り出すことはしない。

利用側のclosureは`GraphError::custom(message)`で独自の失敗を返せる。
エラーは名前とcontextを所有するが、GraphやGPU資源を保持しない。読みやすい`Display`と
`message`は残し、通常の失敗処理に文言解析を要求しない。
構造化contextの追加により、旧`GraphError { message }`という直接構築はconstructorへ移行する。
これは今回の公開API変更であり、既存literalとの互換層は追加しない。
通常の容量不足はstaticなmessageを持ち、名前の複製やcontext用のheap確保を行わない。
`GraphError::capacity()`は確保なしの`GraphCapacityError`を返し、`GraphCapacityKind`で
Graphの各容量、Graph slot、import範囲とそのslotを区別する。`limit`は設定上限、
`required`は判明した必要数であり、解析が個数を報告しない場合や演算overflowでは`None`となる。
slot不足なら既存consumerの返却、metadata不足なら明示準備境界で該当容量の見直しを選べる。
予約自体の失敗でも同じ種別を返すため、上限を増やせば必ず確保できる保証ではない。
不正入力の詳細診断はこの通常backpressure経路と区別して計測する。

native実行の`RuntimeError`／`D3D12RuntimeError`にも`capacity()`を用意し、
共通の`capacity::RuntimeCapacityError`で不足資源、回復方法、判明した上限と必要数を返す。
`CapacityExhausted`だけを見てGPU待機を繰り返さない。未準備、要求自体の上限超過、
GPU進行で戻る枠、未読CPU結果、callerが保持するexport／snapshotは回復操作が異なる。
payloadと通常backpressureのmessageは確保せず、同期・容量検査・失敗時所有を省略しない。
具体的な対処は[容量不足の回復](../guides/development.md#runtime-capacity)に従う。

宣言の範囲・用途・初期化は不要パス除去の前に検査する。
deadなreadも未初期化ならエラーになり、不要な処理で入力ミスを隠さない。
解析専用の`declare_analysis_pass`と実際の操作builderを分け、実行時にliveな解析専用パスを拒否する。
native記録中の任意の実行callbackは提供しないので、Graph構築や除去をCPUのゲーム更新の代わりにしない。

公開`upload_buffer`／`upload_image`は`&[u8]`を借り、構築時に準備済みblobへcaptureする。
呼出し元のbyte列を書き換え・破棄しても完成planの入力は変わらず、compileで再複製しない。
公開`upload_buffer_shared`／`upload_image_shared`と所有Vec入力は削除した。
任意の最後のArcを通常slot返却で解放する経路を避けるため、共有Arc本文は
常駐asset・旧版anchorがあるprivate mesh uploadだけへ限定する。

planはslotの予約領域を保持するため、culledを含む入力本文・名前・access・明示依存が
領域内に残り得る。これはculled passを実行したり、最終内容を初期化済みにする意味ではない。
全plan consumerが終わった後の次の`begin`で領域をresetし、配列の容量は解放しない。
private asset参照の明示保守は`GraphPool::release_idle()`で行い、使用中slotには触れない。
旧版assetの退役は、後述の`MeshRegistry::collect_retired_meshes()`と合わせて扱う。

`graph-diagnostics`だけが追加の依存・寿命の解析情報を保持し、`CompiledGraph::dump()`と
`culled_pass_names()`を提供する。dump文字列は要求時に一度生成し、追加の費用として別計測する。
名前の問い合わせは保持済み領域へのiteratorであり、frameごとの返却Vecを作らない。
論理dumpにはアクセス用途も含む。`graph-diagnostics`構成のVulkanの`submit_with_diagnostics`は、成功した投入と
実際のloweringの診断文字列を返す。通常の`submit`はこの文字列を生成しない。
診断にはpass／論理resource、物理key・世代・native handle、import初期予定状態、
実記録barrierのstage／access／layoutと生成理由、最終予定状態、readbackのHOST_READ依存を含める。
文字列は資源の所有を保持せず、GPU完了tokenでもない。native handle等は実行ごとに変わるため、
安定した機械可読形式や実行間で同一のdumpは保証しない。

`Graph::raster(name, attachments, record)`は宣言時に`record`を実行する。
`RasterPass::draw(&program, draws)`へ`ManagedRasterProgram`とdrawのiteratorを渡す。
配列・sliceと`SnapshotInputs::draws()`は同じ入口を使い、要素は値または`Borrow<RasterDraw>`とする。
各`RasterDraw`はvertex／index buffer・範囲とinstance count・`RasterConstants`・`Option<RasterBindings>`を持つ。
`RasterPass::draw_fullscreen(&program, bindings, constants)`は、頂点・index資源なしで3頂点・1 instanceを記録する。
Programのvertex shaderが頂点ID 0〜2から位置を生成する。Graphがそのdraw専用のbindingと定数を所有し、
shader readsを同じパスへ追加する。attachment、順序、draw容量と未確定のoperandの巻き戻しはindexed drawと共有する。
各Programの色出力形式は`RasterAttachments::colors`のスロット順と一致させる。
`RasterAttachments::depth`は深度アタッチメント、`color_resolves`は色スロットごとのresolve先を指定する。
`depth_only()`はcolorを持たず、depthを要求する。native実行は単一subresource画像を扱う。
各attachmentは画像、`LoadOp<T>`、`StoreOp`を持ち、`Clear(value)`だけがclear値を要求・検査する。
`Load`／`Discard`には未使用clear値を渡さず、native operandには固定値0を使う。
標準mesh引数は`RasterParameters::new(transform, tint)`で有限値を保証し、
`.into()`で80-byteの`RasterConstants`へ変換する。独自shaderの定数は
`RasterConstants::new(&[u32])`で最大128 byteの不変なword列として所有する。
Graph構築ではProgramの定数byte数と一致することを確認し、保証済みのmesh値を再走査しない。
Graphは使用するwordだけをslotの定数配列へコピーする。drawレコードはgeometry、描画引数、binding範囲を保持し、
Programバッチの開始位置とProgram ABIの固定幅で定数を参照する。0ワードのdrawは定数容量を消費しない。
この配置により、indexやbindingを調べる準備処理は小さなdrawレコードを走査できる。
記録は定数を使うバッチで定数配列も順に読み、失敗・unwindはdrawと定数を同時に巻き戻す。
各slotは定数の容量を準備時に予約し、`RasterConstantWords`の容量不足は型付きエラーとして返す。
定数の多い入力では記録時の追加読み取りとバッチの開始位置も費用に含める。
前後比較は[現行検証](../verification/current.md#raster-constant-storage)を参照する。
公開iteratorのsize_hintやExactSizeIteratorの長さを容量の保証には使わず、実際の要素ごとに準備済みdraw容量を確認する。

`renderer-7ft.2.2.4`で公開の単一drawとEngine専用batchを同じowned命令列へ統合した。
`ManagedRasterProgram::mesh()`はビルドに含まれるAPIのmesh VS/FSに結び付き、static descriptorが
頂点stride 28、vertex段のcolumn-major行列＋tint 80 byte、fragment出力location 0を定める。
Device能力はnative PSO作成時に確認する。独自shaderは
`from_trusted_descriptor`で埋め込み記述子を参照するか、`from_trusted_owned_descriptor`でロード済みデータを所有する。
通常のGraph構築・compile・submitはsafeなAPIであり、shader取込の安全性責任とは区別する。
このshaderはdescriptor bindingとstorage副作用を持たない。DXILのb0は同じinline描画定数の表現であり、
このmeshだけから一般graphicsのUniform bindingを推定しない。
`textured_mesh()`は実際の別VS/FSに結び付き、`group 0/binding 0`のVS Uniform先頭16 byteを
clip-space offsetへ使い、`group 1/binding 0`の画像と`binding 1`のsamplerをFSで読む。
stride 28・inline引数80 byte・1 RGBA8出力は同じである。
生成DXILではUniformが`b0/space0`、inline引数が`b1/space0`であり、
meshのinline `b0`と同じnative layoutを流用できる契約ではない。

`mesh_hdr()`/`textured_mesh_hdr()`は同じfloat出力shaderをRGBA16F attachmentへ書くレビュー済み契約である。
`depth_only()`はmeshのvertex shaderと固定深度処理を使い、fragment shaderとcolor出力を持たない。
利用側のtrusted descriptorはcolor出力なしでもdiscard用fragment shaderを持てる。
shaderによるdepth/stencil/sample-mask出力やstorage副作用は引き続き禁止する。
これらのProgram identityとcolor/depthの有無・形式をnative pipelineの識別に含める。
独自descriptorは単一vertex streamのstrideと属性location／offset／Float32x2・x3・x4、または
stride 0と空の属性から成る全画面描画の入力契約を指定する。いずれも
inline定数のbyte数・VS／FS stageとDXIL register／space、読み取りbindingとそのbyte footprint、
color形式、blend、depth状態、両APIのVS／FS binaryを定める。indexed描画のDXILの頂点semanticは
`TEXCOORD`＋location番号とし、indexはu32とする。entryは`main`、どちらもcullingなしのtriangleを使う。
Uniform／StorageRead／SampledImage／SamplerをVS・FSから利用でき、storage書き込みやattachment feedbackは扱わない。
resource descriptorの配列は扱わず、各slotは一個とする。
[D3D12 root constantsの契約](https://microsoft.github.io/DirectX-Specs/d3d/ResourceBinding.html#using-constants-directly-in-the-root-arguments)に従い
inline定数の動的indexは使わない。動的indexが必要な配列はUniform／StorageReadへ置く。
色出力はスロット順に最大8個まで指定でき、Programのblend状態を全スロットへ適用する。
比較sampler、影の照明処理、tone mappingはこの契約に含めない。
両backendの明示的なProgram/画像準備は通常frameの外で行い、capacity不足や未準備のProgramを
通常prepare中のnative作成・内部waitで補わない。各drawのdescriptor内容は記録からcancelまたは実GPU完了まで保持し、
同じpass内でも別drawのUniform/画像/samplerに上書きされない。

planはProgram identity、draw順、各定数、全buffer/View参照を準備済みarenaで所有する。
一つの描画区間に複数のProgramとdraw列を追加できる。全Programの色形式と順序をアタッチメントに合わせ、
Load/Storeとresolveは区間全体に適用する。Programを切り替えても宣言されたdraw順を保持する。

頂点とインデックスの用途はdrawから導き、共有bufferの各用途を集約する。u32三角形の直接描画は
`first_index` と正の `instance_count` を指定でき、base vertexと開始instanceは0とする。
Graphは使うインデックス範囲だけに依存を張る。nativeでは現行内容の範囲保証とDevice上限を確認し、
不変な共有ストリームの全体上限で安全に証明できる場合は毎回の走査を省く。

`IndexedIndirectTable` はインデックス範囲を準備時に確認し、不変のテンプレートとして保持する。
`Graph::encode_indexed_indirect` はテンプレートとGPUの可視性maskを使い、Graph内に20 byteのIndexed Indirect引数を生成する。
maskは各描画のインスタンス数を0または1に決める。公開APIは引数bufferを直接公開せず、
`RasterPass::draw_indirect` がGraph内の型付き引数を受け取る。範囲保証にはインデックスの実内容とDevice上限を用い、
Compute書き込みから間接引数の読み取りまでの同期と資源の寿命をGraphが管理する。
引数の構築に失敗した場合、内部バッファ、exportフラグと追加passを一緒に巻き戻す。同じGraphで容量不足の診断後に構築を続けられる。
集約用range表に加え、各slotでresource上限分の使用index列を準備する。同じbufferは一度だけ列へ登録し、
前回触れたrangeだけを消す。今回の使用indexだけをsortして従来のresource昇順でaccessを追加するため、
非空Rasterごとの全resourceクリア・走査は行わない。draw数D・使用resource数Uに対して
集約・列挙はO(D + U log U)、片付けは前回のUに比例する。slotごとに`usize × resource上限`の
準備領域を追加する交換条件であり、密な入力や実時間性能での優位は未計測である。
構築失敗後も使用列を保持し、次の非空Rasterまたはslot resetでrangeとともに消す。
後半drawの構築失敗でもdraw/access/attachment/nameを戻し、途中のpassを公開しない。
事前に作成したbinding setは維持し、次の非空Rasterで使用markをresetして再利用する。
Engineと利用側は同じ不変`RasterConstants`と公開Raster構築APIを使い、描画引数を再走査しない。
空draw iteratorでもProgramの出力数・形式・必須depthとLoad条件を確認した後、
native Programを使わないattachment-onlyへ正規化する。一般形のclear-onlyは`Graph::attachment_pass`を直接使う。
旧single-draw/batch APIの互換入口は残さない。複数drawも同じLoad/Storeを共有し、Clear/Storeへ暗黙に置き換えない。

Programのidentityはshader・ABI・固定状態を含む不変の版を表す。埋め込み版は静的descriptorの同一性を使い、
ロードした版は取込ごとに再利用しないIDを割り当てる。差し替える際は新しい版を明示準備する。
runtimeのpipeline／layoutとdescriptor leaseは記録・未投入cancel・実GPU完了まで保持する。
Program集合を変更する前に実行を完了させ、結果ハンドルとexport付きsubmissionを返却する。
export／refreshしたbuffer snapshotは同じ登録内容を参照し続ける。D3D12は最後のreadback消費時にnative readback枠を返すが、
結果ハンドル自体の返却はregistryに保持する小さな共有anchorで追跡し、Vulkanと同じ変更境界にする。
anchorは再準備でもregistryとともに保持し、通常frameの結果返却でheapを解放しない。
共通上限は一つのworking set内の64 Program、4 binding group、20 binding、16 vertex属性、128 byteのinline定数であり、
実Device上限とD3D12の64-DWORD root signature上限も準備時に適合確認する。
Vulkanは共有Program参照を無効化せず通常読取にlockを増やさないため、
明示的なRaster集合の準備時に新しい所有者と固定slotを構築し、成功後だけ切り替える。
Compute Program集合は別の準備操作で指定する。Raster集合の変更は未返却consumerがある場合に拒否する。
Rasterは渡した集合だけを有効にし、従来の暗黙meshへのfallbackで除外した資産を再び使える状態にしない。
この準備境界のnative再作成と有限slotの費用を、可変Vecの差替・世代回収の複雑さと比較して選んだ。
上限はruntime生涯の累積資産数ではなく、集合交換後に空きを回復できる。準備失敗は旧集合を保持する。
未使用slotのnative pipelineやdescriptorは作らず、未準備・容量不足で通常frame中に増設しない。

`BlendState::Replace`は置換、`Alpha`はstraight alpha、`PremultipliedAlpha`は乗算済みalphaの合成、
`Additive`は加算を表す。描画順のソートは利用側が行い、Rendererはdraw列の順を保つ。
`DepthState`は無効・任意depth・必須depthを区別し、compare／writeを明示する。
既存prepassed Programは必須depth・LESS_EQUAL・writeなし・Load必須で表し、0 drawでもLoad条件を確認する。
blend対応のformat能力、頂点format、定数stage／size、descriptorとroot layoutは準備境界で確立する。
[Vulkanのblend能力](https://docs.vulkan.org/spec/latest/chapters/framebuffer.html)と
[D3D12のroot visibility／register契約](https://learn.microsoft.com/en-us/windows/win32/direct3d12/creating-a-root-signature)を満たし、
drawごとのABI再走査で代用しない。新しい性能上の優位は主張しない。

Attachmentは選択mip/layer全域をrender areaとする。`Load`は既存内容を要求し、
`Clear(value)`は全域を初期化する。loadの`Discard`は部分的なmesh描画だけで全域を
確立したとみなさない。storeの`Discard`は後続の内容を無効にする。
`Load`／`Clear`と`Store`の組だけが、meshの外側も含めた有効内容を引き継げる。
exportする資源の全域にも有効な内容を要求する。

メッシュは7個のf32（xyz、linear rgba）、stride 28 byte、u32の三角形indexを使う。
CPUのRust構造体を直接キャストせず、各値を明示的にbyteへ変換する。
`GeometryData::new`はindex内容を一度検査し、最大値とcountを不変byte列に対応づける。
privateな管理下uploadはこの保証と既知のprefixを引き継ぐ。全量が確立した通常mesh描画は
index本文を走査せず、頂点数と実Device上限を要約から照合する。検査用のupload Arcも保持しない。
CPU assetや投入前のstaging入力自体の所有は別の必要な寿命である。

任意Graph入力ではruntimeがupload・copyから既知のindex内容を追跡し、index値が頂点数未満であることを確認する。
未知内容や範囲外indexは該当drawの記録前・Graph全体の投入前に拒否し、前半だけをsubmitしない。
VulkanではDeviceの実際の`maxDrawIndexedIndexValue`も確認し、featureを有効化していないことから上限を固定値へ置き換えない。

内容追跡はupload入力の共有sliceを参照し、GPU copyではbyte列をCPUで複製しない。
部分書き換え・copyの未知の穴を保持し、sliceを跨ぐindexも一時bufferを作らず検査する。
既知sliceは開始位置で整列した予約済みsegment配列で管理し、write／copyでは対象区間と
重なる部分を二分探索する。挿入・削除では後続segmentの移動があり、更新は最悪O(S)となる。
このprovenance配列は論理Graphの初期化・hazard解析と別に、native実行用の内容要約を管理する。
heap nodeの確保・解放を避けるための交換条件であり、確保ゼロから更新時間の改善を推定しない。
一般入力のindex検査は同じ内容・index数について最大値を一度求める。
検査したprefixをindex数で索引し、A→B→Aのようにdraw範囲が戻っても同じindex本文を再走査しない。
頂点数／Device上限は保存した内容最大値と比較する。各backendの
`index_prefixes_per_resource`で異なるprefix数を明示準備し、容量不足では要約を追い出して
再走査せず、型付き容量エラーを返す。byte sliceの区間数を制限する
`provenance_segments_per_resource`とは別の上限である。
prefix数Pについて検索はO(log P)、初回はindex本文の走査とO(P)の整列配列挿入を行う。
要約はcountと内容最大値／未知の区別だけで、1件12 byte、既定64件なら768 byteとVec headerを
各provenance storeへ追加予約する。payloadや別の所有者は追加せず、巨大な全prefix表は作らない。
write／copyによる置換で要約を失効し、import snapshotのClone後の更新も独立して扱う。
現在のindex用途とそのcopy元、import／exportしたbufferに必要な内容だけを追跡する。
import／exportしたbufferは後のGraphでindexに使えるため、元の入力を共有したまま保持する。
この保持は新しいCPUミラーの作成とは異なるが、小さなsliceが元の入力全体を保持する場合はある。

管理下要約はprivate markerに結び付け、一般writeでは失効する。位置を保つ完全copyだけが保証を継承する。
全countより短いdrawや部分copyを要約だけから保証済みとみなさず、証明できなければ`UnknownContents`として拒否する。
任意の部分byte更新・copy・将来のindex importに必要なprovenanceは、safe入力の保証として製品にも残る。
全Graph uploadのCPU保持を削除した実装とは説明しない。
[保証の移動](../guides/validation-policy.md#guarantees)と[Graphの分類](../guides/validation-policy.md#graph)を参照する。

## 論理的な依存と実行順

解析は同じslotのresource/pass/access/初期化範囲と明示依存arenaを借用する。
入力sliceとworkspace/outputは互いに重ならないfieldへ分けて借用し、
完成した`CompiledGraph`はslotをpinする。呼出し元のframe借用や自己参照を残さない。

明示依存は予約済みarenaへ追記し、各passのhead/tailとnodeのnext indexで挿入順に辿る。
`depends_on`は対象passの依存数Dに比例する重複照合の後、O(1)で追加する。
後続passのoffsetや既存依存の配列を動かさず、疎な依存chainを二乗時間で構築しない。
満杯でも既存辺の再追加は成功し、新しい辺の容量不足では既存辺を変更しない。
高fan-inの一つのpassへ多数追加する場合の重複照合費用は残る。代わりにnodeのnextと
passのhead/tailを保持する分、旧flat配列より予約metadataが増える。

read/exportのproducerは最終的な依存集合へ直接追加し、readごとの一時集合を作って取り込み直さない。
途中に未初期化の穴があれば解析全体を失敗にし、追加済みprefixの依存を含む不完全な計画を公開しない。
producerの重複排除、内容の有効性、実行されるwriteに対応した最終内容の確定は維持する。
`renderer-7ft.2.2.1`で全accessの一時Vec射影を除去し、予約済み配列のindexを直接使う。

内容のproducerを特定するdataflowと、同じ物理資源のRAW・WAR・WAWによる順序制約は区別する。
不要パス除去はexport・readback・外部に観測される書き込み・明示的な副作用から
必要なproducerを辿る。上書き済みwriterや不要なreaderを、順序制約だけで残さない。
nativeのstage・access・layoutは、残った実際の操作から具体化する。
トポロジカル順序だけではVulkanのメモリ依存は成立しない。

順序依存は資源の区間ごとのwriterとreaderの範囲索引から作り、全先行パスとの総当たりを行わない。
readerはliveなread-only accessにつき一件を、resourceごとの予約済み区間AVLへ記録する。
readでwriterの区間を分割したり、readerを重なる区間数だけ複製したりしない。
同じ範囲のreadは一つの木のkeyへpass履歴を連結する。異なる範囲数Gに対する追加はO(log G)で、
同じ全域だけを繰り返すG=1の追加はO(1)となる。
writerの各区間では、それと重なる先行readのうち、その区間の最後のwriterより新しいものからWARを作る。
部分writeが覆った範囲だけで古いreadを退役扱いにし、別の範囲のWARを失わない。
木はsubtreeの最大終端と最大passを保持して対象外の枝を省く。履歴はcompileごとにresetし、
arenaの予約領域は再利用する。`GraphCapacity::readers`はread-only access数の上限となる。
内容とhazardの区間はそれぞれ、resourceごとのrootを持つ予約済みAVL node arenaへ保持する。
リンクはindexで、削除枠はfree listへ戻す。回転・分割・置換で無関係なresourceや配列の
後続全体を動かさず、offsetの追加順に依存した二乗時間を避ける。通常frameのheap node確保・解放はない。
リンクとproducer/writer等の任意indexは、checkedなone-based `NonZeroUsize`へ格納する。
`Option`を一つのmachine wordで表す。reader索引は範囲と枝刈り用のmetadataを追加保持するため、
旧24-byteの共有連鎖nodeと同じ単価ではない。Windows x64ではreader一件72 byte、resource rootは8 byteで、
writer区間から旧readerリンク8 byteを除いた。必要件数をread数へ抑えることとmetadataの単価を区別する。
writer区間数S、reader数Rに対するqueryは最悪O(S×R)になり得る。read-only区間分割の二次増加を除く修正であり、
全入力の線形時間や一般的なframe時間短縮を約束しない。
32-bitの固定index上限を導入せず、準備済み非ZST Vecが表現できるindex範囲を維持する。
区間数S、対象区間数Kに対し検索と連続読取はO(log S + K)、複数nodeの置換はO((K + 1) log S)となる。
単一区間内の置換は既存nodeを再利用し、容量不足は必要な残存区間数を先に確認してから拒否する。
分割時は既知の前区間とそのsuccessorの間へ挿入し、rootからの再検索をしない。
回転後の親/root接続を確定し、subtree高さが変わらなければ祖先の再balanceを打ち切る。
resourceの全域置換では、そのrootだけを切り離してnodeをpostorderでfree listへ戻し、
一つのnodeへまとめる。部分範囲と別resourceは触らず、再帰深さはAVLの高さに限定する。
少なくとも置換用node一つを回収できるため、切り離し後に容量不足は起こらない。
この場合はO(log S + K)で、古い区間ごとの検索・削除・回転を繰り返さない。
reader索引も所有pointerの連鎖を持たず、履歴の返却で再帰Dropを起こさない。
producerとcullingの意味は維持する。

単純な整列配列に比べ、nodeのリンク・height・rootと木の更新費用が増える。
木の更新費用は残るため、全入力での高速化を主張しない。
全access境界を先に分割する代案は、後のdeadな境界まで初期区間数に含め、
広いwriteを繰り返す入力の走査量とpeak容量を増やすため採用しなかった。
動的な区間数と準備容量の意味を保つ方法を選び、昇順・降順・固定seedの順序と広いreadで比較する。
隣接表・ready heapも予約済み領域を使うが、必要な区間数・依存数自体が増える費用は残る。
開発構成では全宣言の循環を確認し、通常の実行順は残ったパスだけで順序依存を組み直す。
除去された途中のwriter／readerが、必要なパス同士の順序を消さないためである。

hazardと容量検査を終えた全辺が`from < to`なら、liveなpassを宣言順に並べるだけで
従来の最小ready-index順と同じ実行順を得られる。この場合はCSR/入次数/ready heapの構築を省く。
後向き辺や自己依存が一つでもあれば従来の整列・topological・循環経路を使う。
診断構成では直接順序を選ぶ場合も辺を整列し、dumpと試験の順序を維持する。

全passが宣言順のまま残った場合、最初のdataflow解析で求めた内容は最終実行内容と等しいため、
同じwriteを再び区間木へ反映しない。cullingまたは並べ替えがあれば初期内容からliveなwriteを再計算する。
診断用lifetimesは別に求める。いずれもそのframeで構築した入力を解析し直す通常compileの処理で、
固定Graphのplan cacheや未初期化read・hazard・容量検査の省略ではない。

解析専用の`declare_analysis_pass`もsafe入力の保証境界であり、owner・範囲・用途を先に検査してから競合を拒否する。
共有の競合検査は宣言順で最初の衝突pairを選び、read/readや無関係な資源の総当たりをしない。
Aアクセスに対してO(A log A)の処理とO(A)の予約済みscratchを使い、全readならO(A)でsortを省略する。
構築境界と追加検証は同じhelperへ不変なaccess iteratorを渡し、判定algorithmを二重保持しない。
単純な総当たりの参照モデルは試験内で使い、宣言順の診断とread/read許容を独立に照合する。
private builderが保証した入力のcompile再点検は`development-validation`へ分離する。
producer・初期化・culling・live hazard／topological計画・最終内容の確定は製品でも実行する。

<a id="raster-recording"></a>

### 多数の描画をパス間・パス内で準備する

CPU で描画コマンドを作れる時点は、その入力が揃った時点である。例えば影マップの書き込みとそれを読む本描画は GPU 上で順序を持つが、両方の描画引数と資源ハンドルが確定していれば、CPU はそれぞれの描画列を並行して用意できる。GPU の読み戻しで描画数を決める場合は、その値の取得が CPU 側にも必要になる。

パス内でも、順序が決まった描画列を連続するバッチに分け、各ジョブが事前確保した自分の範囲に書ける。合流後に Graph の所有者がパス順・バッチ順に `RasterPass::draw` を呼ぶ。これにより透明物の描画順、同じ画像を上書きする順序、失敗したパスの取り消しを既存の規則で扱える。ジョブの完了順は描画順に影響しない。

件数が既知の indexed draw 列には `RasterPass::draw_generated` を使える。生成関数は、呼ばれた順序に依存せず、指定された番号の描画を返す。Graph は描画数と定数ワード数の上限を先に確認する。`GraphPool::set_raster_jobs` で指定した規模に達した列では、各ジョブが最終格納位置にオペランドと定数を書き、バッファの参照範囲を集約する。異なる binding や離れたインデックス範囲を持つ区間は、保存したオペランドから正確な参照を回収する。ホストが宣言順に依存を登録するため、不透明・透明の順序、範囲の穴、容量エラー時のパス取り消しを保持できる。

`RendererJobs` はホストが用意した一つの `JobSystem` を共有する。標準パイプラインはワールド AABB、カメラ可視性、各カスケードの候補、透明物のソートキーをオブジェクトごとの領域に記録する。影履歴の判定と描画宣言はこの結果を共有する。カメラ外の物体も影を落とす可能性があるため、影候補はカメラと独立に判定する。資源インポート、影履歴の更新、最終的な描画順はホストが管理する。並行する別の Graph が同じチームを使っていれば、その呼出元で処理を進める。

Unreal Engineも、パスごとの描画準備と、可視描画数・コア数に応じたパス内の記録を別のジョブにしている。Khronosのサンプルは、独立した記録先をワーカーに渡し、バッチが小さくなると呼び出し費用が増えることを示す。標準Rendererでもシーン分類、Graph登録、ネイティブ記録を個別に切り替え、完了FPSとフレーム間隔のp95で判断する。[Unreal Engine Mesh Drawing Parallelism](https://dev.epicgames.com/documentation/en-us/unreal-engine/mesh-drawing-pipeline-in-unreal-engine)・[Khronos Command Buffer Usage](https://docs.vulkan.org/samples/latest/samples/performance/command_buffer_usage/README.html)

標準ビューに `prepare_jobs` を設定すると、長い同一プログラムの描画列を最大3,125件の均等な連続バッチに分ける。ネイティブ記録には4人以上のチームを使う。Vulkanは対象8バッチかつ計25,000描画、D3D12は16バッチかつ計50,000描画から分担する。短いバッチ（1,563描画未満）はこの判定から除く。D3D12ではbundleで扱えるバインディングを持つ描画が対象となる。記録先は同時実行枠ごとに64バッチ分を準備する。低レベルのAPIではホストが別の予算や粒度を設定できる。[専用シーンの結果と計測環境](../verification/current.md#standard-renderer-jobs)

標準ビューの自動選択は当該フレームの入力5万物体から適用する。カメラの初回とカット直後は直列で計算し、直前の同じカメラの描画リストと影更新から次回の方式を選ぶ。不透明なForward描画を対象にし、Vulkanではカスケードを更新する不透明シーンも対象にする。影のない不透明PBRは共有メッシュの登録をまとめた最終比較で自動並列化の基準を満たさず、両APIで直列を使う。半透明や矩形光源の影を含むビュー、および採用条件を満たさなかった組み合わせは直列で準備する。方式が切り替わっても当該フレームの入力で境界と可視性を計算する。ビューの外側のGraphは元の登録設定に戻る。

対象ビューのGraph登録では、同一プログラムの連続列が6,144描画以上ならチームで分担する。384〜6,143描画は呼出元でオペランドと定数を最終領域に一括登録し、同じ資源に対する読み取りを集約する。それより短い列は通常の逐次登録を使う。この区切りはネイティブ記録のバッチサイズと独立している。

シーンをバインドする間、メッシュと材質のレジストリは不変であり、同じメッシュ・材質の連続区間では先頭で現在の両リビジョンを検査する。各ビューでは同じメッシュの連続区間につき頂点・インデックスの読み取り参照を一度インポートする。結果の有効範囲はそのGraphと区間に限る。インデックス範囲、ポーズの境界、ライトマップの配置は各物体について解決し、並べ替えは行わない。

性能判断には[大量描画の専用シーン](../guides/development.md#raster-draw-scaling)を使う。実描画総数を固定し、単一の重いパス、複数の独立パス、順序依存を付けた同じパス群で比較する。パス内のバッチ分割も独立に変え、準備と合流の短縮が Graph 登録・コンパイル・ネイティブ記録・GPU 完了を含む処理量にどれだけ残るかを測る。各 API のネイティブ記録まで分ける場合は、確定済みの状態遷移・バリアを元の命令位置に保ち、記録先を排他所有して実際の GPU 完了後に再利用する。[CPU 並列記録の設計条件](../guides/rendergraph-best-practices.md#scheduling)

初期化時に `Runtime::prepare_raster_recording(jobs, max_batches)` を呼ぶと、上記の入力生成とは独立にネイティブ記録を分担できる。ジョブは Graph に登録された連続バッチを単位とする。1回の `RasterPass::draw` に全件を渡せば1バッチであり、重いパスでは順序を保った複数の呼び出しで範囲を渡す。ホストを含めた参加人数と、同時提出に備える1実行枠あたりの記録先の数は独立に指定する。

Vulkan は二次コマンドバッファを実行する。D3D12 は DIRECT リストでバリア・クリア・描画領域・resolve を記録し、専用 allocator と BUNDLE リストに記録した draw を `ExecuteBundle` で実行する。BUNDLE が PSO・root signature・primitive topology を明示する。D3D12 の対象は予算内のバッチ番号で、直接描画の定数・ルートバッファ・頂点・インデックスに対応する。可変の画像 descriptor 表、sampler、indirect を使うバッチは同じ DIRECT リストへ直接記録する。インデックス内容の検査はバンドル実行を登録する前に Graph の更新順で行い、失敗時は未投入の全体を取り消す。GPU の実行順は両 API とも従来の Graph 順とバッチ順を使う。キュー投入は1回で、記録済み draw をホストが再発行する処理はない。[Microsoft のバンドル仕様](https://learn.microsoft.com/en-us/windows/win32/direct3d12/recording-command-lists-and-bundles)

## GPU状態と所有

この節のcanonical identity、内容revision、import／exportの所有は両APIで共有する。
native状態の引継ぎ、cacheとframe APIの詳細はVulkanの契約であり、
D3D12の状態境界と準備済みregistryは[次節](#d3d12-buffer-lifetime)に記す。

<a id="runtime-import-snapshots"></a>

`ImportBuffer`／`ImportImage`のdescriptor・内容revision・初期化状態はprivateで、
runtimeの作成・refresh・exportは共通のchecked constructorから新しい不変snapshotを発行する。
bufferの非ゼロbyte数、画像のextent／mip／layerとbyte数、初期化範囲の境界・順序・非重複を
この発行境界で保証し、画像全体の初期化は全mip×layerの被覆から導出する。
buffer範囲tokenは貸出中に`Arc::get_mut`で書換え不能な`ImportRanges`を保持する。
cloneと通常のGraph importでは保証済みdescriptor／範囲を再走査せず、必要な予約領域へのcaptureを行う。
同じkeyの繰返しimportで異なるdescriptor／revision／内容を渡した場合の拒否は維持する。
keyの検索表はresource上限分をGraphPool準備時に予約する。新規登録は利用側の呼出順で
resource indexを決め、HashMapの列挙順は使わない。key順が交錯しても、整列配列への逐次挿入と
後続要素の移動は発生しない。各keyが一つのresourceを占めるため、resource上限の検査により
通常frameの検索表の拡張も防ぐ。resetはbucketを保持するがcontrol領域のclearを伴う。
整列Vecより準備時のmetadata容量を使う代わりに、平均的な定数時間の検索・追加を選んだ。
これらはsnapshot内部の形式・範囲の保証であり、runtimeのowner／世代／revisionによる
鮮度照合、native容量・実完了・失敗処理を省略するものではない。
write・clear・discard・deleteや世代の再利用はcanonical状態を更新・失効し、
古いsnapshotを書換えず、新しいsnapshotを取得してGraphを構築し直す。

同じruntime資源のimportは、物理資源と状態の同一性を維持する。
exportハンドルの取得はCPUで内容を読めることを意味しない。
単一キューへproducer Graph Aとconsumer Graph Bを投入する場合、
Bは受理済みのAの最終状態を使い、両者の間に必要な依存を記録する。
CPU readbackと再利用には実際の正常完了が必要である。
完了観測は単調なserialだけを保持し、完了した時点の入力byte列を含むsnapshotは複製・保存しない。
最新の予定状態は将来のindex検査に必要な内容を保持するが、全面上書きで不要になった旧入力を
完了情報のために保持し続けない。実際のGPU完了と資源の回収は従来どおりFenceと所有leaseで判定する。

各資源は直近のread範囲だけでなく、後続用途へ可視化する必要のあるwriterのscopeも引き継ぐ。
copyの書き込みを一度copyで読んだだけでは、vertex／index入力へも可視になったとはしない。
bufferのnative依存は現段階では全体を対象に保守的に記録し、
Graphのbyte範囲によるproducer解析と、native barrierの粒度を区別する。

`VulkanRuntime::submit`は実行枠を予約してから、一つのGraphを記録・投入する。
`Submission::export_buffer`／`export_image`は受理済み状態のsnapshotを返す。
これを次のGraphでimportして投入でき、AをCPUで待つ必要はない。
snapshotにはowner・世代・内容revisionがあり、別runtime、削除済み、内容更新前のsnapshotを拒否する。
読み取り専用の利用や画像layoutの変更だけでは内容revisionを進めない。
複数のViewやGraphを先に構築しても、内容が変わっていなければ順次投入できる。
nativeのstage/access/layoutと最後の利用serialは投入時のcanonicalな予定状態から計画し、
古い同期状態を再利用しない。書き込み・clear・discard後は旧内容のsnapshotを拒否する。
更新後は`refresh_buffer`／`refresh_image`で取得し直し、Graphを再構築・compileする。
実行時にもcanonicalな状態と照合するため、compile後の変更を取り逃さない。
owner・世代・内容revisionの照合は製品でも行う。同じ世代のdescriptorは不変で、初期化状態の
変更はrevisionを進めるため、descriptor/初期範囲の全量再比較は`development-validation`だけで行う。

exportした資源はruntimeへ登録され、明示的な`delete`で登録を解除する。
CPUハンドルをdropするだけでは登録を解除しない。
未登録資源は保持参照がなくなってから再利用でき、同じ作成条件ならallocationとViewを保持する。
通常Graph実行は、明示準備済みで同じ作成条件のinstanceだけを借り、不足時は容量エラーを返す。
`prepare_working_set`や`create_buffer`等の明示準備・作成境界では、同じ作成条件がなければ
resource容量までentryを追加し、異なる条件のcacheも保持する。
これらの境界で上限に達した場合だけ、空いている未登録entryのうち最後に受理した利用serialが最も古いものを置き換える。
同じserialならentryのindex順とし、登録中・保持参照があるentry・世代上限のentryを選ばない。
置換先の作成・命名が成功するまで旧entryを維持する。再利用時はnative同期状態を保持し、
以前の内容の初期化情報とCPU provenanceを新しい論理資源へ引き継がない。
resource容量は保持する物理entry数の上限であり、DeviceのVRAM budgetではない。
異なる作成条件を保持する分だけidleメモリが増え、置換時は旧資源と新資源が一時的に共存する。
保持量は論理byteと実際のVMA allocation／blockの統計を分けて観測する。
exportを保持し続けた場合も、容量不足を返し、内部で待機・無制限増設はしない。

`Submission::wait`のtimeoutは`None`で、同じsubmissionを再度待てる。
正常完了後は`SubmissionResult::readback_len`でサイズを取得し、
`readback_into(pass, &mut [u8])`で呼び出し側が準備した領域へコピーする。
CPUコピー成功時にそのstaging領域を解放し、二重消費と別Graphのハンドルを拒否する。
読み取り前の結果を保持していても、完了した実行枠は先に再利用できる。

継続するフレームには`submit_frame`を使える。Runtimeが上限付きで未完了処理を所有し、
返す`FrameSubmission`は`FrameId`とexport snapshotだけを持つ。
このメタデータをdropしても待機・キャンセル・回収は行わない。
`poll_frame`はtimeout 0で実Fenceを観測し、正常完了した一件を`FrameCompletion`として返す。
`wait_frame(timeout)`は結果を保持したまま一件の完了を明示的に待つ。
先に完了済みの結果を調べ、必要なときだけ最古の未完了Fenceを残り予算で待つ。
timeoutや完了を証明できない失敗ではpendingの所有を維持する。
完了したnative枠、upload、descriptor、記録用参照は回収し、未読readbackだけを結果へ残す。
後続の観測で失敗しても、先に返した結果を失わないよう一件ずつ返す。

`pending_frames`は観測前の処理数であり、既にFenceがsignalされていても回収前は数える。
上限は`execution_slots`で、満杯なら投入前に容量不足を返す。
自動waitや無制限な増設でframe数を隠さない。実行枠は従来の`submit`とも共有するため、
frame数が上限未満でも他の利用によって実行枠が不足する場合がある。
終了用の`drain_frames(timeout)`は予算内で明示待機し、完了結果を破棄する。
CPUで必要な結果は先に`poll_frame`で受け取る。予算切れと完了不明の失敗では所有を維持する。
通常のフレーム進行を、従来の`Submission`のDrop待機へ落とさないための追加経路である。

記録から完了まで、使用するbuffer・image・View・pipeline・descriptor・stagingを保持する。
論理的な削除だけでGPU使用中の資源を破棄しない。
記録用の操作はstagingとnative命令に必要な引数を持ち、upload本文を持たない。
正常完了後の結果にはreadback用の保持を残し、記録用の操作は解放する。
投入前のキャンセルや未投入を証明できる失敗と、完了不明の処理を区別する。
timeoutは内容を有効にせず、未完了の所有を維持する。
Device loss後は有効な出力を返さず、新しい仕事を止める。

受理済み状態は投入成功後、ログ出力より先に公開する。
submissionの番号は投入前に消費し、後続のログがpanicしてもrevisionを再利用しない。
結果を返せなかった新規export登録はguardで解除し、実行中の所有はpending側に残す。
native submitのOOMは未投入として扱えるが、それ以外の不確実な失敗はDeviceを停止する。
後始末のFence待機が後に成功しても、未確定の内容を以前の状態として再利用しない。

通常の失敗は`RuntimeErrorKind`で入力・未対応・Renderer容量不足・古い資源・owner不一致・
Device loss／利用停止・native失敗に分類する。
`std::error::Error::source`から元の操作・Device・native結果を調べられる。
native OOMと、設定済みpoolの空き不足は別の分類とする。

<a id="d3d12-buffer-lifetime"></a>

## D3D12の資源状態と所有

`D3D12Runtime::prepare`は、共有`CompiledGraph`のculling後の順序を使ってnative命令を準備する。
producerや範囲の解析をD3D12用に作り直さず、Graph構築で確立した不変の保証を引き継ぐ。
native固有の資源条件・上限と、未対応操作の拒否は準備境界で行う。
不要パスにしか現れない画像・操作は実行しないため、それらだけを理由に拒否しない。

DEFAULT bufferの`COMMON`から、legacy barrierでcopy source／destinationへ状態を追跡する。
Graphはbyte範囲を解析し、native状態遷移は全bufferを対象にする。
同一buffer内の非重複copyは独立したscratch bufferを介し、宣言したbyteの結果を保つ。

`D3D12PreparedGraph`は未投入のcommand・資源を保持し、Dropでキャンセルできる。
`submit`／`submit_prepared`は非同期で、`D3D12Submission`がGPU参照を所有する。
`poll`または`wait(timeout)`が正常完了を返した後だけ、結果の`readback_len`と
`readback_into(pass, &mut [u8])`で利用側の準備済み領域へコピーする。
timeout後も保持して再待機でき、別Graphのhandleとreadbackの二重消費は拒否する。
runtimeを先に終了しても、prepared／submission／未読結果が必要なnative所有者を保持する。

Device removalや投入後のsignal失敗を有効な内容として返さない。
完了不明の処理は停止と所有保持へ進め、未投入の作成・記録失敗と区別する。
Drop時の対象Fence待機と保持の規則は[D3D12 runtimeの設計](implementation-policy.md#d3d12-runtime)を参照する。
実行・資源・CPU結果は準備済みpoolへ分け、未完了submissionの通常Dropは上限付きpendingへ引き継ぐ。

`renderer-7ft.2.5`でbuffer／画像をslot内の専用品から共有物理registryへ移した。
`D3D12Submission::export_buffer`／`export_image`で受理済み内容のsnapshotを取得し、
通常の`Graph::import_buffer`／`import_image`から次のGraphへ接続できる。
同じkeyの重複importは論理資源を増やさず、writer／readerの依存と初期化情報を共有する。
keyのownerはVulkanも含むprocess共通のidentity空間から取得し、別runtime／backendの偶然の数値一致を防ぐ。
`refresh_buffer`／`refresh_image`は新しい予定内容を返し、`delete`は登録とprovenanceを直ちに失効させる。
CPUハンドルのDropは登録解除ではない。記録中・pendingの強い参照が残る物理資源は再利用しない。
再利用は世代を進め、初期化情報とindex provenanceを消す。native資源は同じ準備済みallocationを使う。

内容・範囲・ABIの保証はGraph構築／compileから引き継ぐ。変更可能なowner／世代／revisionはprepareと
submitの境界で照合し、記録後の更新・削除も投入前に拒否する。descriptorと初期化範囲の独立再照合は
`development-validation`でのみ行う。read-onlyのGraphはrevisionを変えず、先に構築・記録した複数consumerを
順に投入できる。liveなwrite／discardは内容revisionを進め、古いsnapshotの再投入を拒否する。
一般bufferが将来indexになる場合のCPU-known provenanceも保持し、shader writeで該当範囲を失効させる。

既存のlegacy barrierによる記録末尾の`COMMON`復帰を維持する。後続Graphは同じDIRECT queueの順序と
明示transitionを使い、producerのCPU完了待機を挟まない。これは[Microsoftの状態遷移契約](https://learn.microsoft.com/en-us/windows/win32/direct3d12/using-resource-barriers-to-synchronize-resource-states-in-direct3d-12)
に従う保守的な基準で、Vulkanのstage/access/layoutをD3D12へ移植せず、barrier削減の性能主張もしない。
RTV／DSVは実行slotの専用descriptorへ借りた画像を設定し、GPU完了前に書き換えない。
logical imageを実行slot内の画像operand枠へ対応づける際も、明示準備したdescription別の
候補列とcursorを使う。classを二分探索して未使用枠を一つ取得し、画像ごとに配列先頭から探索し直さない。
この枠の対応づけと、registryの物理資源lease取得は別であり、importのidentity確認も維持する。
RTV／DSVを設定する対象は生存passのattachmentに使う画像だけを重複なく列挙し、
sampled／Compute／copy専用画像へ使わないtarget viewを設定しない。
候補cursorとattachment列はexclusiveなslotの返却時にresetし、途中失敗・取消でも次のGraphへ残さない。
native実行前に容量とexport tokenを全て予約し、ExecuteとSignal成功後にのみ予定内容と新規登録を公開する。
Signal失敗ではDeviceを停止し、内容を公開せず、実行資源・program・Deviceを含む所有を隔離保持する。
[Fenceの`UINT64_MAX`](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12fence-getcompletedvalue)を正常完了と扱わない。

`D3D12Runtime::new_with_working_set`は`D3D12BufferClass { size, count }`と画像classの
globalな個数を直接準備する。class countへexecution数を自動で掛けない。
`D3D12FrameCapacity::buffers`は各executionのlogical buffer operand枠、`buffer_bytes`は最大logical sizeであり、
物理資源の個数・実sizeは指定classから決める。均一poolを使う`new`の初期buffer数とは区別する。
登録済み資源はこの追加準備分と独立に維持する。`resource_capacity`（既定1024）は登録・未登録cacheを
含むregistry entry総数の上限であり、各Graphの論理資源上限`resources`やbyte budgetとは別である。
bufferはlogical size以上の最小classから空きを選び、画像はmip数も含め完全一致するclassを使う。
小さいlogical bufferへ再利用しても、実際のnative capacityをclass metadataへ保持する。
空き候補は準備済みのclass別indexで管理し、登録中のentryは候補から除外する。
buffer classをbinary searchとprepared bitmap/cursorで選び、各classのcursorは未登録候補を最大一回確認する。
登録済みprefixやregistry全体を資源ごとに再走査しない。
登録・delete・世代上限・working set再準備でindexを更新し、実際のArc参照数・世代・descriptorを
最終的な貸出し条件として維持する。cursor通過後に参照が返った候補は次のrecordingから使えるため、
現在のbindが一時的な容量不足を返す場合がある。内部waitや候補領域の増設は行わない。
代償はregistry容量分のnode/class metadataで、同classのlease保持候補の線形確認は残る。
CPU探索と所有取得・返却の比較は[現在の検証](../verification/current.md#runtime-review-improvements)へ記録する。
通常のprepareは登録中・参照中の物理資源、実行枠、結果枠、export metadata、range tokenの不足を
容量エラーとして返し、増設もGPU待機もしない。`initialized_ranges_per_resource`と`range_snapshots`は
個別に指定でき、途中のtoken取得失敗でも既に取得した参照を全て返す。
working set変更はVulkanと同様に全実行・結果・export付きsubmissionの返却を必要とする。
登録資源のkey・revision・初期化範囲・provenanceと、発行済みの`ImportBuffer`・`ImportImage`を維持する。
buffer snapshot用のpoolはregistryとともに存続し、固定の枠数に既存のtokenを含める。内容更新やdeleteによる失効は従来どおりである。
新classに含めなかった登録画像も各executionのview枠へ含め、native画像を余分に複製しない。
準備に必要なnative資源・Program・descriptorを別poolに全て作成してから、旧registryの再利用可能な
未登録indexへ新資源を移し、最後にregistryを新poolへ移す。登録entryは動かさず、未登録indexの世代を
次の貸出しで進める。明示したfree class budgetへ縮小する場合は余剰の未登録idle allocationをこの準備境界で退役させ、
`Some(&[])`は準備時点のfree buffer数をゼロにする。登録資源は保持し、後の明示deleteによるpool返却は別の変更操作である。
native/CPU consumerの返却前に物理ownerを早期解放しない。容量不足・通常の準備失敗で旧登録を失わず、
resizeの反復でregistryを追加し続けない。Device removal等による停止は従来どおり終端状態として扱う。

`D3D12Runtime::memory_stats()`は明示した観測境界で、registryの登録中・未登録保持中・再利用可能な論理byteと
準備済みtransfer容量を取得する。`memory-profiling`が有効なときだけ、Device単位で全committed資源の
native確保要件とalignment別件数を保持する。Graph外のmanaged資源と退役中の所有も集計する。
DXGIの同adapter・同processのlocal/non-local使用量・budget・reservationは別に照会し、
resourceの論理byteや確保要件から推測しない。live staging使用量の未観測値は`None`で残す。
観測は一時的なDXGI interface作成・scanを含み得るので、通常frameのゼロ確保窓へ入れない。
feature無効時の専用metadata除外と測定の意味は[Bistroの常駐受入](implementation-policy.md#bistro-residency)に従う。

registryを複数pool世代で共有する案より、返却済みの変更境界で値を移す方式を選ぶ。
pending中の再準備を追加せず、共有寿命・退役世代・新しい回収APIを増やさないためである。
GraphのUNORM/float color画像はtransfer／sampled／RT／UAVの固定用途とし、初回Program集合に関わらず後のstorage利用を許す。
`Rgba8Srgb`はsample/transferだけに限定し、RT/UAV用途を加えない。
Microsoftは[UAV flagによるlayout効率への影響](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/ne-d3d12-d3d12_resource_flags)も説明する。
用途限定画像と必要時のcopyを使う代案より、Vulkanと同じ資源の再利用契約を優先した基準である。
copyによる移行・新しいGPU transactionは導入せず、速度やmemory効率の優位は未測定とする。
画像準備ではtyped UAV view、Program作成では実際のtyped load/store能力を確認する。depthはDS用途を維持する。
登録画像専用slotの転送footprintは、native作成と同じdescriptionから
[`GetCopyableFootprints`](https://learn.microsoft.com/en-us/windows/win32/api/d3d12/nf-d3d12-id3d12device-getcopyablefootprints)で得る。
strict／shippingの内容照合と変動frameの確保測定は[現在の検証](../verification/current.md)を参照する。

## 同期と一般推奨の適用

[ベストプラクティス](../guides/vulkan-best-practices.md#synchronization)に従い、
Copy、Compute storage、fragment sampling、vertex/index入力、color/depth attachment、
readbackのHOST_READを実際の用途へ対応づける。
Dynamic Renderingのパス間barrierはrendering scopeの外へ置く。
readbackがない処理にはHOST_READ依存を追加しない。

単一キュー、保守的な同期、通常のdescriptor set、固定shaderは正しさを確認する基準である。
vendor依存の高速化や高度なパス統合は採用しておらず、性能改善を主張しない。
利用側独自shader/pipelineとパスAPIはB0/B1で優先して設計・実証する。
複数キューと物理aliasingはBistro実測から採否を決める後続候補であり、同じ優先度へまとめない。

<a id="windows-presentation"></a>

## Windows表示との接続

`presentation`はWindow adapterを追加し、backendを暗黙に選ばない。
`vulkan,presentation`または`d3d12,presentation`を有効にする。
両APIは同じGraph構築、入力行列、出力を使い、runtimeとPresenterだけを選択する。
表示専用のGraph操作や別のscene登録を要求せず、swapchain画像を公開しない。

`Runtime::for_window` は `HasWindowHandle + 'static` の Win32 ウィンドウ所有者と
`PresentationConfig` から Runtime と Presenter を返す。`Arc<winit::window::Window>` やエンジンの
`Rc` ウィンドウを直接渡せる。event loop、camera、入力は利用側が管理する。
同じ Device・queue・登録簿を使う二つ目以降の出力は `Runtime::create_presenter` で追加する。
最初のウィンドウより前に資源を準備するときは `Runtime::new_with_presentation` を使う。Vulkan では
全ウィンドウで共有する表示用実行枠をここで予約し、D3D12 は各 Presenter の作成時に枠を用意する。
同じ HWND を他の描画 API と同時に使用せず、別の Presenter に渡す場合は元の Presenter を破棄する。
各 Presenter を個別に poll する。共通 API の `prepare` は旧世代の退役待ちを `WindowReadiness::Pending` で返す。
`ExecutionErrorKind::SurfaceLost` を受け取った場合は当該 Presenter を閉じ、新しいウィンドウを既存 Runtime に接続する。
configは同時表示枠`max_frames`、退役世代上限`max_retired_swapchains`、
2〜16の希望画像数`desired_images`とVSync要求を持つ。
実Surfaceの制限に合わせた画像数と、Graph実行枠・表示slot・image indexは別概念である。

両Presenterは`prepare`、`extent`、`resize`、`set_vsync`／`vsync`、`poll`、
`wait_idle(timeout)`、`close(timeout)`と明示的な旧世代回収を同じ意味で提供する。
`prepare`で変更を進め、readyなextentと同寸法の完全なlinear RGBA8画像をGraphでexportして
runtimeの`present`へ渡す。同queueへの別submitでsRGB出力へ変換し、Graph完了のCPU waitは挟まない。
HDR表示、拡大縮小、tone mapping、transparent Windowはこの入口に含めない。

共通の `WindowPresenter::submit_frame(&mut runtime, &compiled, output)` はGraphを一度投入し、
export済み画像をWindowに預ける。`present_frame(&mut runtime)` が `NotReady` を返したら、
同じWindowをpollしてから `present_frame` を再試行する。描画の再投入は不要である。
recipe経由では `DisplaySdr::export_for_display` でGraphに画像を指定し、
受理したreceiptを `queue_frame` へ渡す。Windowごとの未表示画像は一枚で、
別の画像への差し替えや表示中止には `discard_frame` を使う。

`Graph::export_frame_image` で指定した画像は、共通の `Runtime::submit` が返すreceiptから
`take_frame_image` で取得できる。直接投入時は `Runtime::submit_with_image` でも
同じ `ManagedImage` を取得できる。Graphが新規作成した画像は、その所有者または未消費receiptを
破棄すると元のRuntimeの次のsubmit、poll、または `collect_released_images` で登録が外れる。
返却枠は投入前に確保し、上限は `RuntimeLimits::resource_capacity` とする。
登録をアセット管理側へ移す場合は `into_registered` で受け取り、不要時に `Runtime::delete` を呼ぶ。
既存画像をimportして描いた場合は、元の呼び出し側が登録の寿命を管理し続ける。
いずれも表示やGPU実行が受理された後の物理画像は、それぞれの完了までbackendが保持する。

| 共通の`PresentOutcome` | 利用側の扱い |
|---|---|
| `Presented { suboptimal }` | 表示要求の受理。source登録を解除できるが、画面への到達やnative資源の退役を意味しない |
| `NotReady` | poll後に再試行する。暗黙wait／容量増設を加えない。先に受理した描画submitが残る場合がある |
| `Recreated` | 変更完了。extentを取得し直し、旧出力を新寸法のGraphへ置換する |
| `RecreateRequired` | 変更が必要。prepareを進めるまで旧extentを新しい保証として使わない |
| `Suspended` | 希望extentがゼロ。acquireせず、resizeによる復帰を待つ |

VSync ON／OFFは同期する／しないという要求であり、未対応のモードへ黙ってfallbackしない。
OFFはVulkanのIMMEDIATE、D3D12のtearing対応を要求する。OS compositorやdriverの実表示は別に扱う。
枠不足は型付き`CapacityExhausted`、nativeの一時的な非readyは`NotReady`として返す。
closeのtimeoutを完了扱いにせず、未完了のsource・command・Window所有を保持する。

sourceのowner・内容revision・全域初期化・format・extentは表示へ接続する境界で確認する。
読み取りだけでは内容revisionを変えない。内容write／deleteやextent変更は古い入力の保証を失効させる。
表示submit受理後はsourceの強いnative所有を実GPU完了まで保持し、表示拒否から未投入rollbackを推定しない。
Vulkanはcanonicalな予定scope/layoutを更新し、D3D12はCOMMON復帰を共有registryへ引き継ぐ。
Vulkanのpresent fence、DXGIが保持する表示参照、アプリのGPU Fence、画面への到達は異なる条件である。
API固有の証明は[VulkanのWSI設計](vulkan.md#windows-presentation)と
[D3D12表示](implementation-policy.md#d3d12-presentation)へ閉じ込める。

標準recipeと独自Graphを`windowed`の同じcamera・event loopへ接続する。
通常frameは固定容量のGraph／native／結果slotを再利用する。
resize／VSync変更／profiling有効化は明示変更境界であり、古いconsumerが残る間はpoll・再試行する。
メモリ規約にはbuild／compileから表示、poll／退役、readback結果消費・全slot返却までを含める。
Graph GPU区間と表示用GPU区間、CPU acquire／present費用を分け、display latencyへ読み替えない。
操作・再実行は[開発手順](../guides/development.md#windows-presentation)を参照する。

## 履歴と段階的な拡張

履歴は永続keyからfreshなsnapshotを取得し、前のGraphの予定状態を次のGraphへimportする。
前フレームのCPU完了待機は不要だが、内容を保持する履歴を単なる一時画像として貸し出さない。
読み取りだけなら内容revisionは変わらず、保存済みsnapshotからの複数consumerを構築できる。
内容を書き換える履歴chainでは、受理された更新後にfreshなsnapshotを取得する。
初回と明示reset、extent変更では内容を初期化し直す。
旧履歴の登録は最後に使用するGraphの投入後に解除でき、pending側の所有が完了まで維持する。
二枚交互という番号だけでは安全性を証明せず、予定アクセスと最後の利用を追跡する。

M3のエンジン入力は、scene/ECSの意味、Graph構築、native記録、GPU寿命を分けて接続する。
custom passの挿入はGraph構築時に行い、標準描画と同じproducer解析・内容検査・同期を通す。
現時点の実行操作は管理下のshaderに限定するが、依存解析に固有のshader実装を持ち込まない。
現行Programはshader/pipelineの所有と、宣言されたABI・binding metadata・Graph宣言の整合を作成／Graph構築／準備境界で確認する。
shaderの全入力に対する動作安全性は利用側がunsafe取込契約で保証する。固定操作単独の検証を一般Programの保証へ流用しない。
公開パスの形はB0で複数の利用コードから設計し、B1以降のVulkan/D3D12実行で確認した。
独自の効果を追加するためにGraph本体やeffect列挙型を変更しないことを受入条件とする。
構築時に所有された命令を蓄える方式と実行callback方式は、B0の利用コード比較から前者を選んだ。

代表利用コードによる[公開API比較と第一案](implementation-policy.md#public-api-comparison)では、
型付きbinding／attachmentからアクセスを導き、構築時に命令を所有する案をB1の出発点とした。
frame内借用が可能な遅延callbackとも同じ利用例で比較した。CPU試作は当時検討した独自program、
depth-only／color-only、HDR、MRT／mip等の契約を検討するもので、上記の現行対応範囲を拡張しない。
基盤は[B0の比較](implementation-policy.md#foundation-selection)でnative上位置換＋低レベル再利用を選んだ。
今後の拡張でもproducer・import・寿命契約を維持し、変更時は理由と検証を記録する。

## 公開標準パスと標準グラフ

`sashiko_renderer::passes`は公開Graph操作を使う構築関数を提供する。
Unlit Forward、PBR Forward、Forward+、Deferredは公開パスを組み合わせるレシピである。
レシピはGraphを借用してパスを追加する。unlit Forwardの入口は`RasterPass::draw`と同じowned／borrowed `RasterDraw`のiteratorを受け取り、
`SceneColor { color, depth }`というGraph内のhandleを返す。
export、readback、compile、submit、presentと、その結果の所有は呼び出し側が選ぶ。

| 部品 | 入出力と意味 |
|---|---|
| `passes::opaque` | callerのcolor attachmentと任意depthへ頂点色×tintを描く。RGBA8／RGBA16Fから公開mesh Programを選び、Clear／Load／Storeを引き継ぐ |
| `passes::depth_prepass` | callerのD32 attachmentへ同じ頂点変換の深度だけを描く。colorを確保しない |
| `unlit_forward::forward` | 指定したcolorと必要ならD32を作り、clearとopaqueを追加する。`ForwardConfig`はframeをまたいで再利用できる |
| `unlit_forward::PreparedForward` | `ForwardConfig`と最大draw数に応じた要求を`Preparation`へ登録し、フレームごとに`SceneColor`を構築する |
| `pbr_forward`／`pbr_forward_plus` | 材質、太陽光、IBLを使う1×／4×レシピを準備する。RGBA16FのHDR、任意の4カスケードの影とSDR出力を構築する。Forward+は点光源をタイルごとに選別する |
| `pbr_deferred` | 1×のG-bufferと照明パス、後段のForward半透明描画を構築する |
| `pbr_deferred::passes` | `reserve`でG-bufferと照明パスの要求を追加し、`append`で利用側のGraphへ宣言する。出力先は同寸で1×、各1 mip・1 layerのRGBA16FとD32を渡す |
| `passes::output::tone_map_neutral` | scene-linear RGBA16Fに露出倍率とKhronos Neutralを適用し、同寸のdisplay-linear RGBA16Fへ書く |
| `passes::output::write_sdr` | 指定したcolor viewから同寸のRGBA8_UNORMへ書く。`SdrOutput`でFXAAの有無と出力encodingを選ぶ |
| `passes::output::PreparedOutput` | 寸法と`SdrOutput`に応じた要求を登録し、HDR入力にトーンマッピングとSDR変換を追加する。トーンマッピング後の線形画像とSDR画像を返す |

準備済みレシピは一時資源とProgram、Graph宣言数を`sashiko_renderer_core::capacity::Preparation`へ登録する。
独自パスの要求も同じ準備に加えられる。
[準備と使い方](../guides/tooling.md#rust-pipeline)を参照する。

両出力パスは`RasterPass::draw_fullscreen`を使う。頂点入力のないtrusted Programが3頂点を生成し、Graphは頂点・インデックスbufferへの依存を追加しない。
標準出力とDeferredの照明パスは、`sashiko_renderer_core::program::fullscreen_triangle_shaders`を通じて同じ管理下の頂点shaderを使う。
標準出力のfragment shader、ABI、露出・FXAA・SDRの選択は `sashiko_renderer::passes::output` が所有する。
独自の出力方式も同じ頂点shaderと `draw_fullscreen` を利用し、自分のProgramのtrusted取込でABIとアクセス範囲を保証する。
`tone_map_neutral_pipeline()`と`SdrOutput::pipeline()`を使って実行前に対応するProgramを準備する。
ウィンドウには`SdrEncoding::Linear`で書いた画像を渡し、PresenterがsRGB変換を行う。オフスクリーンのSDRデータには`SdrEncoding::Srgb`を選ぶ。
Bloomなど露出前の効果はscene-linear画像に接続し、トーンマッピングとSDR変換の間にも任意のGraph処理を組み込める。

unlit Forwardは頂点色とtintから出力色を求める。入力のlinear色をRGBA8_UNORMへ書く場合は0〜1へ量子化し、
RGBA16Fでは1超や負値も保持する。HDR形式を選ぶだけでPBR、露出、tone mappingやHDR表示を実装したことにはならない。
Unlit Forwardは1×を扱う。PBR Forward／Forward+は1×でシーンのD32深度を返し、4×ではカラーのみを単一サンプルHDRへリゾルブする。
4×のシーン深度は`None`になる。すべてのレシピで2D・mip/layer各1・通常Zを使う。
`opaque`単体はcolor-onlyも許す。recipeの`depth: false`ならdepthは作らず、post入力のdepthも`None`となる。
必要なdepthがなければ利用側は明示的に拒否するか別の処理を選び、暗黙の代用画像を作らない。

深度はclip 0〜W、通常Z、D32、clear 1を基準とする。`OpaqueDepth::Write`は既存のLESS＋depth write、
`Prepassed`はLESS_OR_EQUAL＋depth writeなしの公開`mesh_prepassed`／`mesh_hdr_prepassed`を選ぶ。
後者は0 drawでもdepth必須・Load必須であり、先行prepassと同じView、geometry、変換・描画範囲を利用側が与える。
Graphのdepthアクセスは保守的なattachment read/writeのままであり、read-only同期の最適化を主張しない。
Loadは初期化済み内容を保持し、Clearは0 drawでも初期化する。Discardや不十分な描画被覆を初期化済みにはしない。
未初期化LoadとStore Discard後のreadは通常のGraph構築／compile契約で拒否する。

`passes::opaque`は渡されたdraw順を維持し、alphaは出力へ格納する。
PBR Forwardは不透明・マスク描画の後にalpha blendを行い、透明な描画の順序は入力に従う。
標準PBRの材質係数は型付きアップロードで登録し、そのアップロードから得たアルファ分類を使って
不透明・マスク・半透明の入力先を検査する。材質の係数を変更するときはアップロード境界から新しい入力を使う。
通常フレームでGPUの係数を読み戻したり、分類を求めるために資源のバイト列を走査したりしない。
表示用の線形SDRと保存用のsRGBは、どちらもRGBA8 UNORMに格納し、標準出力の戻り値では用途を型で区別する。
表示には受理済みexportを使う。通常のGraph依存、資源の版、GPUの寿命管理はどちらの出力にも適用する。
独自のblend・depth設定を使う描画部品は、trusted Raster descriptorと`Graph::raster`で定義できる。
独自graphics shaderや任意stateの提供を、固定Program群や標準部品の公開だけで完成扱いにしない。

`forward`が返すscene-linear colorやdepthを入力にして、追加のパスを同じGraphに構築できる。
ここで`graph`は利用側が準備したGraph、`inputs`は任意のsnapshot接続である。

```rust,ignore
use sashiko_renderer_core::graph::ImageDesc;
use sashiko_renderer::unlit_forward;

let config = unlit_forward::ForwardConfig {
    color: ImageDesc::rgba8(1280, 720),
    clear_color: [0.0, 0.0, 0.0, 1.0],
    depth: true,
};
let scene = unlit_forward::forward(&mut graph, "forward", inputs.draws(), config)?;
let output = if let Some(effect) = effect {
    effect.append(&mut graph, scene.color)?
} else {
    scene.color
};
graph.export_image(output)?;
```

effect固有の出力formatやextentは利用側のパスが決める。別画像へ同format・同extentの内容が必要なら`Graph::copy_image`を使う。
別構造のrendererは公開標準パスと独自Computeなどを直接組み合わせる。
標準部品固有の設定誤りは公開`GraphError::custom`で説明し、owner・容量・内容等のGraphエラーはその分類を保つ。
多段構成の失敗後は、部分構築されたGraphを破棄するか宣言済みの状態と残り容量を考慮して利用する。
未使用の部品・中間画像は通常のcullingに従う。部品が隠れたexportや副作用rootを追加することはない。

[標準構成fixture](../../crates/sashiko-renderer-core/fixtures/standard_fixture.rs)は、同じframe入力について
標準forward直接、identity post、利用側多段Computeのpost、標準パスからの直接合成を示す。
postは`Graph::dispatch`で既存の利用側sampled乗算／storage加算Programを接続し、
順序の交換・追加加算・無効化を選ぶ。postを一つのGraphノードや固定effect列挙へ制限しない。
二API共通の独自Computeには、このProgram／dispatch経路を使う。

定義・設定とframeのdraw／行列／材質を分け、毎frame必要なGraphを構築・compileする。
callbackは保持せず、完成命令は入力を所有する。準備済み容量内でVec等の追加確保をせず、
必要なProgramとnative working setは明示準備する。新しいprepassed Programも通常frame中の遅延pipeline作成を許さない。

## エンジン入力と標準描画

**現行実装:** 利用側がGraphから構築を始め、必要なsnapshotだけを接続できる。
`sashiko_renderer::unlit_forward::snapshot`は標準opaque、単一出力と任意のreadbackを構築する便利経路を提供する。
そのRGBA8・View寸法の契約は標準描画の便利経路で適用する。入力接続は利用側の任意の構成で使える。
[公開標準パスと標準グラフ](#公開標準パスと標準グラフ)を同じ入力接続から選択できる。

GPU非依存の`geometry::MeshRegistry`は`MeshCapacity { meshes, retired_mesh_versions }`で
meshと変更済み旧版の容量を準備する。`MeshRegistry::new`の資源登録容量と、
frameごとのdraw数・同時snapshot数の容量は別である。
型付きハンドルはowner・index・generationを検査し、更新は同じハンドルの内容を進め、
削除と再登録では世代を分ける。meshは GeometryData の不変バイト列を保持する。UnlitのDrawはlinear RGBA tintを値として渡す。
scene、ECS、asset loader、カメラの意味は呼び出し側が所有する。
RendererはModel構造を持たず、mesh/materialと描画入力を扱う。
Modelを含めない責務境界と理由は[実装方針](implementation-policy.md#21-ゲームエンジンへ提供するもの)を参照する。

`SnapshotPool::new(draw_capacity, slots)`を準備し、
`unlit_forward::FrameSnapshot::capture(&meshes, &pool, &view, &draws)`で可変の`View`・`Draw`をcaptureする。
`FrameSnapshot`はmeshの版とtint・行列を所有し、
後続のCPU更新・削除や一時借用に依存しない。`Matrix4`はcolumn-major、column vectorとし、
`clip_from_world * world_from_model`を合成する。clip depthは0〜W、viewport高さは正、通常Zである。
Unlitのdrawパラメーターはsnapshot作成時に `RasterParameters` を介して `RasterConstants` に確定し、owned Raster構築で再走査しない。
利用側も同じ生成時保証を使い、途中失敗時はRaster passを追加しない。
snapshotはCloneせず、必要な同時frame数のslotを準備する。capture途中の失敗と通常Dropは
draw配列・mesh解決用scratchをpoolへ返す。元のEngineとSnapshotPoolは通常処理の間保持し、
これらを先に終了して残存snapshotへ最後のheap解放を移す処理は終了境界として扱う。
投影とhandednessは入力行列で選ぶ。Rustのstruct layoutをGPU ABIとせず、
頂点は明示byte化し、描画引数も64-byte MVPと16-byte tintへ明示的に変換する。

### 利用側からの入力接続と出力選択

GPUなしでは`GraphPool::begin()`から構築を開始し、`snapshot.connect_graph(&mut graph)`で
`SnapshotInputs`を得る。接続はmesh bufferとuploadだけを追加し、Raster・画像・export・readbackは追加しない。
`Graph::raster`のクロージャ内で`raster.draw(&program, inputs.draws())`を呼ぶ。
Program・描画先・Load/Store・後続処理は利用側で定める。
使わないuploadは通常のcullingで除去する。複数snapshot/Viewや同じmeshの異なる版も接続できる。
入力leaseとsnapshotをdropした後も、構築済み命令は定数と必要なmesh streamを保持する。

接続のmesh解決配列はsnapshotの準備済み領域から一時的に借りる。user code中にmutexを保持せず、
同じsnapshotへ同時に二つ目の接続を要求した場合は`SnapshotInputs`容量不足（上限1）を返す。
`inputs`のDropで配列を返し、その後は同じsnapshotを再接続できる。失敗・unwindでも配列は返る。
接続途中のGraph容量不足では一部の未使用buffer/uploadが残り得るため、Graphをcancelするか、
残る容量内で構築を続ける。既存命令は変更せず、暗黙の容量拡張や待機をしない。

Vulkan常駐入力では`VulkanEngine::begin_graph()`が`PreparedEngineGraph`を返し、
`connect_snapshot(&mut prepared, &snapshot)`で同じ`SnapshotInputs`を得る。接続時に必要版をpinし、
`compile()`の`CompiledEngineGraph`、submit/cancelまで保持する。
接続cacheはmesh handleとrevisionで区別し、複数Viewで版を混同しない。
常駐していない版は`NotResident`、prepared枠/mesh接続容量不足は型付き不足を返し、uploadやwaitへfallbackしない。
この所有型は既存Graph/CompiledGraphと準備済み常駐pin leaseを包み、別のGraph解析や実行経路を持たない。

接続したmesh bufferはGraph内で読み取り専用とし、upload・copy先・shader write・宣言writeを拒否する。
常駐版の不変性をexport経由で破れないよう直接のbuffer exportも拒否する。
必要なら独立bufferへcopyしてその結果を書き換え/exportする。owner/世代、canonical import、index provenanceは通常の保証を維持する。

任意個数の画像/buffer出力は`prepared.graph.export_*`／`readback_*`で利用側が選択し、
`prepared.compile()`と`VulkanEngine::submit_graph`で通常の`FrameSubmission`へ進める。
新規に作ったexportの登録はconsumerを投入後に利用側が`runtime.delete(key)`で解除する。
importした出力の登録は元の所有者が管理する。中間資源の一律exportは行わない。
一画像の便利経路は構成後に`prepared.select_output(image, readback)`を選ぶ。
形式や寸法をViewへ合わせる制約はなく、初期化・handleはGraphの通常の契約で検査する。
元のGraph・選択出力・Frame/Callerの解放権限を確定して、既存の`submit_frame`／`release_output`へ接続する。
GPU完了後までのnative保持は通常のRuntimeの仕事であり、frame metadataのDropを完了とはしない。
一般D3D12 import/履歴は`.2.5`で別途受け入れた。このEngine接続だけを二APIのEngine完成とは扱わない。

### 現行の標準描画便利経路

`sashiko_renderer::unlit_forward::snapshot::build_graph(&snapshot, &mut graph_pool, readback, extension)`は
GPU・windowなしで標準Graphを構築できる。
この単独経路はmeshをGraph内でuploadするため、小さい入力やCPU解析用とする。
mesh解決はsnapshot slotの予約済み配列とsort/index検索で行い、frameごとの一時BTreeMapを作らない。
`sashiko_renderer_core::graph::FrameGraph::select_output(graph, output, readback)`は、使用するGraphと画像から
最終出力・export・任意readback・登録解除責任を確定する。`readback`には必要な場合だけパス名を渡す。
任意の描画構成でこのAPIを使える。
`graph()`は読み取り専用で、`output()`／`readback()`／`output_ownership()`も確定時の対応を保つ。
拡張と出力の選択は`build_graph`のcallback内で済ませる。任意の後続構築が必要なら
`connect_graph`と通常のGraph APIを使う。公開fieldの差し替えで所有情報だけが古く残る経路は設けない。
`FrameGraph::compile(self)`は同じbackingを不変planへ移し、構築Graphを残すための複製を行わない。
持続的な実行では、利用側が `MeshRegistry` と取得したスナップショットを所有し、`VulkanEngine`は
runtimeとGPU常駐を所有する。同じsnapshotを独立したrendererへ渡すことができ、
バックエンド別のCPU asset登録は要求しない。

meshの更新・削除は、既存snapshotが持つ版と両byte streamのanchorを
予約済みretired領域へ移してから公開状態を進める。retired容量不足なら変更を拒否し、
現在版・handleを部分的に更新しない。外側の`Arc<GeometryData>`だけでなく、
private uploadや複製した`GeometryData`が持つvertex/index streamも旧版の保持条件に含める。
変更に紐づく明示保守では、idleなGraph slotのpinを`GraphPool::release_idle()`で返し、
`MeshRegistry::collect_retired_meshes()`が最後の必要なstream anchorまで確認して退役させる。
streamを共有する複数の内部版があっても、外部consumerが残る最後のanchorを失わない。
通常frameの入力配列返却を保守例外へ移す方式ではない。

GPU常駐はmeshのowner・index・generation・revisionで識別し、`MeshCacheConfig`で件数と
論理byte数を制限する。CPU assetの削除と別に`unload_mesh`で常駐を解除でき、
新しいworking setの準備では、必要でなくpinもない常駐meshを決定的な順に退避する。
必要working set自体が予算を超える場合と、一時的なpending資源の容量不足を区別する。
`prepare_snapshot(snapshot, non_mesh_resources)`では、合成側がmesh以外に残す登録容量を指定する。
buffer-onlyなら0、現行の標準Viewではcolor/depth用に2を指定する。これは描画先の固定や物理予約ではない。
指定された余裕を含む物理容量も入場判定に使い、論理mesh予算内でも
古い登録が描画先の枠を埋めるときは退避する。prepared pinで必要枠を確保できなければ明示エラーを返す。
準備済みframeは必要な登録をpinし、submit後のnative所有は通常のGPU leaseへ引き継ぐ。
CPU更新・削除後の旧snapshotも、そのimmutableな版として独立して準備できる。

`unload_mesh`は対象数を返して論理退役をmarkし、`unload_all_meshes`と`cancel_uploads`もmarkのみを行う。
旧assetのMap node・native登録・CPU sourceの実回収は、資源変更に紐づく明示的な
`VulkanEngine::collect_retired_meshes(Option<&MeshRegistry>)`へ分離する。通常のsubmit/pollは
frame・execution・readbackのleaseを従来どおり返すが、旧assetを最後に解放しない。
準備済みframeがpinする登録は保守でも維持し、回収待ちentryもmesh予算へ含める。
新assetの入場に不足する場合は、明示的な保守と再試行を呼び出し側で選ぶ。

`MeshCacheConfig::max_retired_sources`はGPU mesh件数と独立したCPU source退役容量で、
各sourceのbyte上限と合わせて保持量を制限する。満杯なら型付き不足を返し、増設・内部waitを行わない。
`Some(&assets)`は同じCPU Engineの現在版/退役版が同じ不変入力を保持することをidentityで確認し、
そのEngineの退役保証へanchorを引き継ぐ。CPU Engineは通常処理中保持し、残存consumerを伴う先行終了は終了境界で扱う。
`None`または別ownerでは、外側とvertex/index両streamのconsumerが終わるまでsourceを保持する。
これにより二つの退役poolの相互待ちを避けつつ、CPU入力の最後の解放を通常frameへ移さない。

`prepare_snapshot(snapshot, non_mesh_resources)`と`poll_uploads`は明示的に分割uploadを進める。
一つのmeshや全入力がstaging容量を超えても、同じ上限付きpoolを再利用する。
`Ready`は必要なbyte列がすべてqueueへ受理されていることを意味し、GPU完了ではない。
内容の版ごとに元のimmutableな入力を共有し、chunkごとのCPUコピーを永続的な鏡像として保持しない。
同じqueueの後続描画はCPU完了待ちを要求しない。通常の準備・poll・build・submitに暗黙waitを入れず、
明示的な待機には`wait_uploads(timeout)`を使う。uploadの回収が別の描画frameの結果を消費しないよう、
所有するsubmissionを分ける。`sashiko_renderer::unlit_forward::snapshot::build_vulkan`は準備済み常駐を使う。
同モジュールの`render_vulkan`は構築・compile・submitまで扱い、uploadと待機は明示した準備経路で行う。
Fence完了後の結果・計測取得だけが失敗した場合も、退役済みuploadを回収して元のエラーを返す。
完了不明のFenceエラーやtimeoutは同じ回収扱いにせず、所有を維持する。
標準の不透明描画は管理下mesh Programとowned draw listを一つの`Raster`操作にする。
空draw listはclear-only、非空ならcolor/depthを一回clearし、宣言された順に全drawを描いてStoreする。
Rasterノード数はdraw数によらず一件で、custom Compute・明示的なcomposite・readback等は別ノードのままである。
同じmeshの再利用と、異なるmeshの組合せの両方を扱い、drawの行列・tint・index数を所有する。
compileは同じslotのdraw範囲を凍結し、必要なconsumerが終わるまでplanのpinで所有を維持する。

外部bufferアクセスは資源ごとに範囲を集約し、同じbufferがVertexとIndexの両方なら両用途を残す。
indexは各drawの頂点範囲とDevice上限について記録前に検査し、後のdrawが不正でも部分的にsubmitしない。
canonical importは`ResourceKey`をkeyとするprivateな索引で検索し、同一性とsnapshot整合性の検査を保つ。
Vulkanの用途集約も論理資源indexの準備済み配列と利用index列を使い、多数の異なるmeshを一つのパスへ集めても全入力対を走査しない。

単一drawの比較は同じ公開APIで一drawずつpassを作り、複数drawの一passと出力を照合する。
標準opaqueは管理下mesh shaderを明示する契約で、公開PassEncoderや任意パスの自動統合ではない。
別の描画先、Compute、copy等の必要な境界と同期は維持する。
共有mesh／異なるmeshの画像・同期・性能の確認は[検証記録](../verification/current.md#draw-listの本実装と比較計測)を参照する。

拡張点はopaque描画後の完全なlinear RGBA8画像である。
Viewと同じextentで初期化済みの画像を返し、通常のGraph builderで処理を追加する。
closureはGraph構築時に実行され、GPU記録中にsceneへ再入しない。
返却画像のhandle・形式・寸法・初期化はGraphの通常の検査を通す。
不要なcustom branchも通常どおり除去される。
返された完成画像を直接exportし、無変換の最終compositeと別画像を毎Viewに強制しない。
`FrameOutputOwnership::Frame`はこのGraphで作った出力、`Caller`は利用側がimportした出力を表す。
`release_output`はframe所有の元のkeyだけを登録解除し、caller所有の登録には触れない。
CPU経路では`FrameGraph::compile()`、持続的な実行経路では`PreparedEngineFrame::compile()`で
構築物を消費して凍結する。後者はmeshのpinと出力の所有権を`CompiledEngineFrame`へ引き継ぎ、
`VulkanEngine::submit_frame(compiled)`が同じ所有付き入力を受け取る。
build、compile、submitを分けられる。利用側の履歴処理や測定を標準描画と同じGraphへ接続できる。
準備済みframeとplanのGraph identity、元の出力と必要exportを投入前に照合する。
buildはGPU実行枠の予約ではない。内容が変わる別submitや外部keyの削除ではimportが失効するが、
共有meshの読み取りだけを行う別Viewのsubmitでは失効しない。
`RenderedFrame`のframe所有出力は明示解放まで登録されるため、表示や履歴等のconsumerを投入後に
`release_output`を呼ぶ。CPUメタデータのDropに暗黙の登録解除を結び付けない。
