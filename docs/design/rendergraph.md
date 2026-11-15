# RenderGraphの契約

全体像と具体例は[図で読むRenderGraph](rendergraph-overview.md)、
検索・章ナビゲーション付きHTMLの生成は[文書ガイド](../guides/html-docs.md)を参照する。

Graphの宣言・解析はGPUとウィンドウに依存せず、通常のCPU構成で利用する。
Vulkan実行は`vulkan` featureで追加し、公開APIへash・VMAの型を出さない。
Windowsの`d3d12` featureは同じ`CompiledGraph`のbuffer upload・copy・readbackを実行する。
Graphの宣言・解析と、各backendが実行できる操作の範囲は区別する。
全体方針は[実装方針](implementation-policy.md)、native資源と完了管理は
[Vulkanの設計](vulkan.md)と[D3D12の設計](implementation-policy.md#d3d12-runtime)、
実行済みの確認は[検証状況](../verification/current.md)を参照する。

検証処理の配置・製品構成からの除去には[製品版と開発用の検証方針](../guides/validation-policy.md)を適用する。
以下は現在のAPI・動作の契約である。backend選択、追加検証、診断保持は独立に選ぶ。

次の開発は[実装方針のBistro計画](implementation-policy.md#bistro-roadmap)に従う。
B0で実シーンの要求・公開API利用例と[基盤の採用・移行範囲](implementation-policy.md#foundation-selection)を設計した。
B1では構築時owned命令とbinding/attachment由来アクセスを一般化し、nativeの低レベル所有を再利用して二APIで実証する。
現行の固定操作、内部表現、構築順による意味論、runtimeの再利用は維持義務ではない。
以下の現行契約は、変更を実装・検証するまで有効であり、計画だけで対応済みとは扱わない。

## ソースの責務と依存方向

`graph.rs`は公開型のfacade、Graph所有状態、identityとcompile入口を持つ。
子moduleの`resource`は論理資源・import・範囲、`pass`は宣言とowned命令、
`error`はprivate contextを持つ構造化エラー、`builder`は検査後のtransactionalな追加を担当する。
`storage`は準備済みslotと借用範囲、`ranges`はimport初期化範囲の不変snapshotを所有する。
`accesses`/`analysis`は予約済み作業領域で解析し、`compile`が構築領域を不変planへ凍結する。
既存builder回帰群は`graph/tests.rs`へまとめる。
公開型はfacadeから直接reexportし、並行する互換APIを作らない。

Graphから`engine`の型を参照しない。`MeshData::upload_graph_slice`は不変なstreamと
既存のindex要約を選び、Graphの狭いprivate builderがowner/全stream長/alignment/範囲を検査して追加する。
engine側からGraphのpassを直接変更せず、任意のpublic byte uploadからmesh provenanceを作らない。
元のArc、slice位置とappend失敗時の状態を維持し、chunkごとのpayload複製や追加scanを入れない。

## 対応範囲と処理の境界

Vulkanの経路は、単一Graphics＋Computeキュー、物理aliasingなし、
2D画像のmip・layer・sample各1を対象とする。
これはnative実行の範囲である。論理Graphは以下のmip/layer単位のCPU計画も扱う。
bufferは半開byte範囲を指定する。
論理ハンドルと物理資源の識別、内容の有効性と画像layout、
予定されたGPU状態と実際の完了を別々に管理する。

通常のパスは、管理下のcopy・Compute・Attachment clear・indexed mesh・最終合成という限定した操作を宣言する。
任意の生CommandBuffer、外部SPIR-V、ネイティブ外部ハンドルは受け入れない。
これにより、実行する操作と宣言した用途・範囲・bindingを対応づける。
汎用RHIや未信頼shaderのサンドボックスを提供する契約ではない。
依存するCompute処理は別ノードとし、パス間の同期を実行基盤が記録する。

| 実行基盤 | 現在の範囲 |
|---|---|
| Vulkan | buffer／画像copy、固定Compute、Attachment clear、indexed mesh、合成、import／export、上限付き再利用 |
| D3D12 | transient bufferのupload・copy・readback。単一DIRECT queue、committed resource、物理aliasingなし |

D3D12のprepareはexportとliveなimport・画像・未対応操作を明示エラーにする。
共有Graphを構築できることだけで、すべてのbackendがその操作を実行できるとは扱わない。

B1の最初の範囲として、Graph外の`program::ManagedProgram`と両runtimeの
Program、Uniform/Storage buffer、RGBA8/RGBA16F/D32画像/View、nearest/clamp samplerの共通binding作成APIを用意する。
Viewはsampled用とD32のdepth attachment用を区別し、aspectはformatから導出する。
bindingはmanifest順に
実Programと全資源の版/範囲を保持する。Storage本文はGPU可変で、作成を初期化とは扱わない。
`StorageRead`と`StorageWrite`は同じStorage資源のbinding用途を表す。
private受入のproducer→consumer同期を、公開Graphの一般dispatch対応とは扱わない。
[管理下資源の契約](vulkan.md#managed-uniform)を参照する。
この作成APIとprivate GPU受入を、公開Graphの一般draw/dispatch対応とは扱わない。
一般owned pass・attachment/subresource由来の計画は`renderer-7ft.2.2`で進める。
その作成領域・解析workspace・不変planの所有は、以下の準備済みslotへ移行した。

### 管理下buffer Computeのowned計画

`renderer-7ft.2.2.2`では`Graph::dispatch(name, &program, &bindings, workgroups)`を追加した。
`ManagedProgram::uniform_readback()`と`storage_readback()`、`[1, 1, 1]` workgroupsを扱い、
sampled assetや他のdispatch数は構築時に`Unsupported`として拒否する。
`graph::BufferBinding`にはgroup/binding、論理buffer、offset/sizeだけを指定する。
用途はProgramから導き、別のread/write一覧を要求しない。

命令は不変な管理下Programのidentity、dispatch数、manifest順に正規化したbindingを所有する。
Programはprivateなstatic manifestを参照し、bindingは準備済みarenaへ値として格納するため、
呼出元のProgram変数・binding sliceを変更/破棄しても完成planは変わらない。
これは論理bufferの命令であり、`.2.1`のnative Program/Bindingsをimportした所有契約ではない。

descriptor全範囲の非ゼロ・ABI最小size・論理buffer内の境界を構築時に確認し、全offset/sizeを保持する。
解析にはshaderの実際のread/write footprintだけを使う。現在の二assetはbinding先頭16 byteを読み、
出力先頭16 byteだけを初期化する。32 byteのdescriptorを渡しても残り16 byteを初期化済みとしない。
この保証はABIの`min_size`から推定せず、レビュー済みshaderと同時管理するprivate実行契約に置く。
shaderやdispatch条件を変える際はfootprint・初期化保証も改訂する。
Uniform readは`AccessPurpose::Uniform`であり、StorageReadへ代用しない。
同passでwriteを含む実footprintが重なるbindingは拒否し、descriptorだけが重なる非重複範囲は区別する。

初期化・宣言順producer・RAW/WAR/WAW・culling・循環は既存の共通解析へ接続する。
`GraphCapacity::bindings`が全passのbinding上限で、超過は`GraphCapacityKind::Bindings`として返す。
途中のslot/range/access/name/capacity失敗はbindingとaccessの追加を戻し、通常frameでheapを増設しない。

native実行は後続`.2.4`であり、両backendはliveなmanaged dispatchを明示拒否する。
culled dispatchは実行を妨げず、その論理buffer用のnative資源も取得しない。
Device固有のalignment・usage・descriptor/dispatch上限、native Program/bindingの所有、
D3D12のresource単位の状態互換性はCPU計画だけでは保証しない。
MRT、画像subresource、一般Raster、一般Computeの二API実行を`.2.2.2`の完了に含めない。

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

CPU assetとsnapshotの準備は後述の`EngineCapacity`と`SnapshotPool`が担当する。
native実行枠・物理資源・readback結果の準備は各backendの所有契約で別に管理する。
全段階の達成可否は[通常フレームの規約](implementation-policy.md#steady-state-memory)と
[検証状況](../verification/current.md)で確認し、この共通所有契約だけから
native作成・破棄や表示・結果消費を含む全体のゼロ達成を主張しない。

## producer・範囲・Attachment

### 画像subresourceとowned attachment配列

`renderer-7ft.2.2.3`。`ImageDesc`は`mip_levels`と`array_layers`を持ち、
`rgba8`/`depth32` constructorは各1を設定する。mip数は最大extentから許される段数まで、
layer数は非ゼロとし、全mipの縮小extentとlayer数のpacked byte合計をchecked演算で検査する。
これはnative allocation sizeやVRAM使用量の見積もりではない。

`Graph::image_view(image, mip, layer)`は同じ画像の単一subresourceを指す不変な値を返す。
owner・範囲を作成境界で確認し、aspectをRGBA8のColor／D32のDepthから導く。
`ImageView`は画像を別resourceとして登録せず、複数Viewの重なりを元の画像で解析する。
現行は単一mip/layer Viewであり、複数mip/layerを覆う一つのViewは未対応である。
論理解析は`layer * mip_levels + mip`の区間へ写像し、producer・未初期化拒否・RAW/WAR/WAW・cullingを共通解析へ接続する。
`declare_pass`の画像範囲も同じ各subresourceを扱う。エラーでは最初の未初期化subresourceをmip/layer/aspectへ戻す。
`export_image`は全subresourceの有効内容を要求する。既存native importのinitialized boolは画像全体を表し、nativeは今も各1だけを生成する。

`Graph::attachment_pass(name, RasterAttachments)`は描画命令を持たないattachment操作を追加する。
color配列は0個以上、depthは任意だが、全体が空の指定は拒否する。
各`Attachment { view, load, store }`を準備済みarenaへcopyし、呼出元のsliceを変更/破棄してもplanは変わらない。
選択mipのextentは全targetで一致させ、色/深度の形式、clear値、owner、同一subresourceへの重複writeを構築時に拒否する。
同じ画像の非重複layerは別のtargetにできる。Load/Store/Discardは選択subresourceだけに適用し、
loadがDiscardの操作だけで有効な内容を確立したとは扱わない。
`GraphCapacity::attachments`はcolorとdepthを合わせた保持数の上限で、失敗時には途中のarena/access/name追加を戻す。
旧`clear_attachments`はこのAPIへ置換し、互換入口を残さない。

Vulkanへlowerできるのは従来の1 RGBA8＋D32・単一subresource・両方Clear/Storeの形だけである。
この形は構築時に確定して保持し、native記録で入力配列の保証を再走査しない。
color-only/depth-only/MRTやmulti-subresource画像のlive使用はnative資源の取得前に`Unsupported`を返す。
資源準備と画像cacheのlookup前にもmulti-subresourceを拒否し、古い1mip画像を誤って再利用しない。
culledな未対応枝は実行や物理資源取得を妨げない。D3D12の画像実行、一般Rasterとmulti-subresource native実行は後続に残る。
既存のwhole-image upload/copy/readback/Compute/compositeは単一subresource画像だけを受け入れ、
一部の操作を画像全体の初期化として扱わない。

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

宣言の範囲・用途・初期化は不要パス除去の前に検査する。
deadなreadも未初期化ならエラーになり、不要な処理で入力ミスを隠さない。
解析専用の`declare_pass`と実際の操作builderを分け、実行時にliveな解析専用パスを拒否する。
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
旧版assetの退役は、後述の`Engine::collect_retired_meshes()`と合わせて扱う。

`graph-diagnostics`だけが追加の依存・寿命の解析情報を保持し、`CompiledGraph::dump()`と
`culled_pass_names()`を提供する。dump文字列は要求時に一度生成し、追加の費用として別計測する。
名前の問い合わせは保持済み領域へのiteratorであり、frameごとの返却Vecを作らない。
論理dumpにはアクセス用途も含む。`graph-diagnostics`構成のVulkanの`submit_with_diagnostics`は、成功した投入と
実際のloweringの診断文字列を返す。通常の`submit`はこの文字列を生成しない。
診断にはpass／論理resource、物理key・世代・native handle、import初期予定状態、
実記録barrierのstage／access／layoutと生成理由、最終予定状態、readbackのHOST_READ依存を含める。
文字列は資源の所有を保持せず、GPU完了tokenでもない。native handle等は実行ごとに変わるため、
安定した機械可読形式や実行間で同一のdumpは保証しない。

`Graph::raster(name, &program, draws, attachments)`は明示的な`ManagedRasterProgram`と借用draw sliceを受け取る。
各`RasterDraw`はvertex／index buffer・index count・`RasterParameters`をまとめ、
attachmentsは`colors: &[Attachment<[f32; 4]>]`と`depth: Option<Attachment<f32>>`を区別する。
現在の管理下mesh Programは1 RGBA8出力を持ち、CPU計画ではdepthを省略できる。
Vulkanのnative実行は従来の1color＋depth・単一subresource画像だけを扱い、他の形は物理取得前に`Unsupported`にする。
各attachmentは画像、`LoadOp<T>`、`StoreOp`を持ち、`Clear(value)`だけがclear値を要求・検査する。
`Load`／`Discard`には未使用clear値を渡さず、native operandには固定値0を使う。
既定の描画引数には`RasterParameters::default()`を明示する。

`renderer-7ft.2.2.4`で公開の単一drawとEngine専用batchを同じowned命令列へ統合した。
`ManagedRasterProgram::mesh()`は実在する両APIのmesh VS/FSに結び付き、private static manifestが
頂点stride 28、vertex段のcolumn-major行列＋tint 80 byte、fragment出力location 0を定める。
これはnative PSO作成やDevice能力の保証ではなく、任意source/binaryや自己申告のmanifestを受け入れない。
このshaderはdescriptor bindingとstorage副作用を持たない。DXILのb0は同じinline描画定数の表現であり、
一般graphicsのUniform binding/stage導出や新しいGraph資源bindingを実装したとは扱わない。

planはProgram identity、draw順、各定数、全buffer/View参照を準備済みarenaへ所有する。
vertex/index用途はdrawから導き、共有bufferの各用途を失わずに集約する。Indexはu32三角形、
first index/base vertexは0、instance数は1だけを扱う。CPU計画だけでindex本文の範囲内や実Device上限を保証せず、
既存のnative provenance・更新時の失効・未知内容の拒否を維持する。
後半drawの構築失敗でもdraw/access/attachment/nameを戻し、途中のpassを公開しない。
Engineはsnapshotで確立した有限値のprivate保証を同じcoreへ渡し、描画引数を再走査しない。
空draw sliceはProgramを使わないattachment-onlyへ正規化するため、空の場合はattachment配列の一般形も扱える。
旧single-draw/batch APIの互換入口は残さない。複数drawも同じLoad/Storeを共有し、Clear/Storeへ暗黙に置き換えない。

Attachmentは選択mip/layer全域をrender areaとする。`Load`は既存内容を要求し、
`Clear(value)`は全域を初期化する。loadの`Discard`は部分的なmesh描画だけで全域を
確立したとみなさない。storeの`Discard`は後続の内容を無効にする。
`Load`／`Clear`と`Store`の組だけが、meshの外側も含めた有効内容を引き継げる。
exportする資源の全域にも有効な内容を要求する。

メッシュは7個のf32（xyz、linear rgba）、stride 28 byte、u32の三角形indexを使う。
CPUのRust構造体を直接キャストせず、各値を明示的にbyteへ変換する。
`MeshData::new`はindex内容を一度検査し、最大値とcountを不変byte列に対応づける。
privateな管理下uploadはこの保証と既知のprefixを引き継ぐ。全量が確立した通常mesh描画は
index本文を走査せず、頂点数と実Device上限を要約から照合する。検査用のupload Arcも保持しない。
CPU assetや投入前のstaging入力自体の所有は別の必要な寿命である。

任意Graph入力ではruntimeがupload・copyから既知のindex内容を追跡し、index値が頂点数未満であることを確認する。
Deviceの実際の`maxDrawIndexedIndexValue`も確認し、確認できない内容や範囲外indexを
native命令の記録前に拒否する。featureを有効化していないことから上限を固定値へ置き換えない。

内容追跡はupload入力の共有sliceを参照し、GPU copyではbyte列をCPUで複製しない。
部分書き換え・copyの未知の穴を保持し、sliceを跨ぐindexも一時bufferを作らず検査する。
既知sliceは開始位置で整列した予約済みsegment配列で管理し、write／copyでは対象区間と
重なる部分を二分探索する。挿入・削除では後続segmentの移動があり、更新は最悪O(S)となる。
このprovenance配列は論理Graphの初期化・hazard解析と別に、native実行用の内容要約を管理する。
heap nodeの確保・解放を避けるための交換条件であり、確保ゼロから更新時間の改善を推定しない。
一般入力のindex検査は同じ内容・index数について最大値を一度求める。
頂点数／Device上限が変わってもこの要約を比較し、同じindex本文を再走査しない。
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

順序依存は資源の区間ごとのwriterと未処理readerから作り、全先行パスとの総当たりを行わない。
reader履歴はindexで参照する不変のarenaへ保持し、区間の分割では履歴を共有する。
writeで複数区間のreaderを辿る際も共有tailを重複して走査しない。履歴はcompileごとにresetし、
arenaの予約領域は再利用する。長い連鎖の破棄で再帰する所有構造にはしない。
内容とhazardの区間はそれぞれ、resourceごとのrootを持つ予約済みAVL node arenaへ保持する。
リンクはindexで、削除枠はfree listへ戻す。回転・分割・置換で無関係なresourceや配列の
後続全体を動かさず、offsetの追加順に依存した二乗時間を避ける。通常frameのheap node確保・解放はない。
リンクとproducer/writer/readerの任意indexは、checkedなone-based `NonZeroUsize`へ格納する。
`Option`を一つのmachine wordで表し、現行Windows x64測定の内容/hazard nodeは各64 byte、reader履歴は24 byteとなる。
32-bitの固定index上限を導入せず、準備済み非ZST Vecが表現できるindex範囲を維持する。
区間数S、対象区間数Kに対し検索と連続読取はO(log S + K)、複数nodeの置換はO((K + 1) log S)となる。
単一区間内の置換は既存nodeを再利用し、容量不足は必要な残存区間数を先に確認してから拒否する。
分割時は既知の前区間とそのsuccessorの間へ挿入し、rootからの再検索をしない。
回転後の親/root接続を確定し、subtree高さが変わらなければ祖先の再balanceを打ち切る。
resourceの全域置換では、そのrootだけを切り離してnodeをpostorderでfree listへ戻し、
一つのnodeへまとめる。部分範囲と別resourceは触らず、再帰深さはAVLの高さに限定する。
少なくとも置換用node一つを回収できるため、切り離し後に容量不足は起こらない。
この場合はO(log S + K)で、古い区間ごとの検索・削除・回転を繰り返さない。
reader履歴は共有indexを保持し、長い履歴の返却で所有連鎖の再帰Dropを起こさない。
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

解析専用の`declare_pass`もsafe入力の保証境界であり、owner・範囲・用途を先に検査してから競合を拒否する。
共有の競合検査は宣言順で最初の衝突pairを選び、read/readや無関係な資源の総当たりをしない。
Aアクセスに対してO(A log A)の処理とO(A)の予約済みscratchを使い、全readならO(A)でsortを省略する。
構築境界と追加検証は同じhelperへ不変なaccess iteratorを渡し、判定algorithmを二重保持しない。
単純な総当たりの参照モデルは試験内で使い、宣言順の診断とread/read許容を独立に照合する。
private builderが保証した入力のcompile再点検は`development-validation`へ分離する。
producer・初期化・culling・live hazard／topological計画・最終内容の確定は製品でも実行する。

## GPU状態と所有

この節のcanonical state、import／export、cacheとframe APIは現在のVulkan実行の契約である。
D3D12の限定したbuffer実行の所有と完了は[次節](#d3d12-buffer-lifetime)に記す。

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

## D3D12のbuffer状態と所有

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
この段階にはcanonical import／exportと永続keyを追加していない。
実行・資源・CPU結果は準備済みpoolへ分け、未完了submissionの通常Dropは上限付きpendingへ引き継ぐ。

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
任意shaderを導入するときはshader/pipelineの所有、binding metadata、宣言との照合、
ABIと信頼境界の検証が追加で必要になる。現在の限定操作だけでそれらまで保証したとは扱わない。
公開パスの形はB0で複数の利用コードから設計し、B1でVulkan/D3D12の実行結果を確認する。
独自の効果を追加するためにGraph本体やeffect列挙型を変更しないことを受入条件とする。
構築時に所有された命令を蓄える方式、実行callback方式などは比較して選び、記録時期や型名を先に固定しない。

代表利用コードによる[公開API比較と第一案](implementation-policy.md#public-api-comparison)では、
型付きbinding／attachmentからアクセスを導き、構築時に命令を所有する案をB1の出発点とした。
frame内借用が可能な遅延callbackとも同じ利用例で比較する。CPU試作は将来の独自program、
depth-only／color-only、HDR、MRT／mip等の契約を検討するもので、上記の現行対応範囲を拡張しない。
基盤は[B0の比較](implementation-policy.md#foundation-selection)でnative上位置換＋低レベル再利用を選んだ。
実装変更までは現行のproducer・import・寿命契約を維持する。

## エンジン入力と標準描画

GPU非依存の`engine::Engine`は`EngineCapacity { meshes, materials, retired_mesh_versions }`で
mesh/materialと変更済み旧版の容量を準備する。`Engine::new`の資源登録容量と、
frameごとのdraw数・同時snapshot数の容量は別である。
型付きハンドルはowner・index・generationを検査し、更新は同じハンドルの内容を進め、
削除と再登録では世代を分ける。meshは不変の入力byte列、materialはlinear RGBA tintに限定する。
scene、ECS、asset loader、カメラの意味は呼び出し側が所有する。
RendererはModel構造を持たず、mesh/materialと描画入力を扱う。
Modelを含めない責務境界と理由は[実装方針](implementation-policy.md#21-ゲームエンジンへ提供するもの)を参照する。

`SnapshotPool::new(draw_capacity, slots)`を準備し、
`Engine::snapshot(&pool, &view, &draws)`で可変の`View`・`Draw`をcaptureする。
`FrameSnapshot`はmeshの版とtint・行列を所有し、
後続のCPU更新・削除や一時借用に依存しない。`Matrix4`はcolumn-major、column vectorとし、
`clip_from_world * world_from_model`を合成する。clip depthは0〜W、viewport高さは正、通常Zである。
有限なdrawパラメーターはsnapshotでprivateな検証済み値として確立し、owned Raster構築で再走査しない。
public `Graph::raster`のraw入力検査は維持する。Engineのowned operandは同じcoreでfallible iteratorから直接作り、
途中失敗時はpassを追加しない。
snapshotはCloneせず、必要な同時frame数のslotを準備する。capture途中の失敗と通常Dropは
draw配列・mesh解決用scratchをpoolへ返す。元のEngineとSnapshotPoolは通常処理の間保持し、
これらを先に終了して残存snapshotへ最後のheap解放を移す処理は終了境界として扱う。
投影とhandednessは入力行列で選ぶ。Rustのstruct layoutをGPU ABIとせず、
頂点は明示byte化し、描画引数も64-byte MVPと16-byte tintへ明示的に変換する。

snapshotの`build_graph(&mut graph_pool, readback, extension)`はGPU・windowなしで標準Graphを構築できる。
この単独経路はmeshをGraph内でuploadするため、小さい入力やCPU解析用とする。
mesh解決はsnapshot slotの予約済み配列とsort/index検索で行い、frameごとの一時BTreeMapを作らない。
取得した`FrameGraph.graph`は消費型compileへ渡し、構築Graphを残すための複製を行わない。
持続的な実行では、利用側の`Engine`がCPU assetとsnapshotを所有し、`VulkanEngine`は
runtimeとGPU常駐を所有する。同じsnapshotを独立したrendererへ渡すことができ、
バックエンド別のCPU asset登録は要求しない。

meshの更新・削除は、既存snapshotが持つ版と両byte streamのanchorを
予約済みretired領域へ移してから公開状態を進める。retired容量不足なら変更を拒否し、
現在版・handleを部分的に更新しない。外側の`Arc<MeshData>`だけでなく、
private uploadや複製した`MeshData`が持つvertex/index streamも旧版の保持条件に含める。
変更に紐づく明示保守では、idleなGraph slotのpinを`GraphPool::release_idle()`で返し、
`Engine::collect_retired_meshes()`が最後の必要なstream anchorまで確認して退役させる。
streamを共有する複数の内部版があっても、外部consumerが残る最後のanchorを失わない。
通常frameの入力配列返却を保守例外へ移す方式ではない。

GPU常駐はmeshのowner・index・generation・revisionで識別し、`MeshCacheConfig`で件数と
論理byte数を制限する。CPU assetの削除と別に`unload_mesh`で常駐を解除でき、
新しいworking setの準備では、必要でなくpinもない常駐meshを決定的な順に退避する。
必要working set自体が予算を超える場合と、一時的なpending資源の容量不足を区別する。
通常Viewのcolor/depth二資源を含む物理容量も入場判定に使い、論理mesh予算内でも
古い登録が描画先の枠を埋めるときは退避する。prepared pinで必要枠を確保できなければ明示エラーを返す。
準備済みframeは必要な登録をpinし、submit後のnative所有は通常のGPU leaseへ引き継ぐ。
CPU更新・削除後の旧snapshotも、そのimmutableな版として独立して準備できる。

`unload_mesh`は対象数を返して論理退役をmarkし、`unload_all_meshes`と`cancel_uploads`もmarkのみを行う。
旧assetのMap node・native登録・CPU sourceの実回収は、資源変更に紐づく明示的な
`VulkanEngine::collect_retired_meshes(Option<&Engine>)`へ分離する。通常のsubmit/pollは
frame・execution・readbackのleaseを従来どおり返すが、旧assetを最後に解放しない。
準備済みframeがpinする登録は保守でも維持し、回収待ちentryもmesh予算へ含める。
新assetの入場に不足する場合は、明示的な保守と再試行を呼び出し側で選ぶ。

`MeshCacheConfig::max_retired_sources`はGPU mesh件数と独立したCPU source退役容量で、
各sourceのbyte上限と合わせて保持量を制限する。満杯なら型付き不足を返し、増設・内部waitを行わない。
`Some(&assets)`は同じCPU Engineの現在版/退役版が同じ不変入力を保持することをidentityで確認し、
そのEngineの退役保証へanchorを引き継ぐ。CPU Engineは通常処理中保持し、残存consumerを伴う先行終了は終了境界で扱う。
`None`または別ownerでは、外側とvertex/index両streamのconsumerが終わるまでsourceを保持する。
これにより二つの退役poolの相互待ちを避けつつ、CPU入力の最後の解放を通常frameへ移さない。

`prepare_snapshot`と`poll_uploads`は明示的に分割uploadを進める。
一つのmeshや全入力がstaging容量を超えても、同じ上限付きpoolを再利用する。
`Ready`は必要なbyte列がすべてqueueへ受理されていることを意味し、GPU完了ではない。
内容の版ごとに元のimmutableな入力を共有し、chunkごとのCPUコピーを永続的な鏡像として保持しない。
同じqueueの後続描画はCPU完了待ちを要求しない。通常の準備・poll・build・submitに暗黙waitを入れず、
明示的な待機には`wait_uploads(timeout)`を使う。uploadの回収が別の描画frameの結果を消費しないよう、
所有するsubmissionを分ける。`build_snapshot`は準備済み常駐を使い、uploadや待機を隠して実行しない。
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
CPU経路では`FrameGraph.graph.compile()`、持続的な実行経路では`PreparedEngineFrame::compile()`で
構築物を消費して凍結する。後者はmeshのpinと出力の所有権を`CompiledEngineFrame`へ引き継ぎ、
`VulkanEngine::submit_frame(compiled)`が同じ所有付き入力を受け取る。
build、compile、submitを分けられる。利用側の履歴処理や測定を標準描画と同じGraphへ接続できる。
準備済みframeとplanのGraph identity、元の出力と必要exportを投入前に照合する。
buildはGPU実行枠の予約ではない。内容が変わる別submitや外部keyの削除ではimportが失効するが、
共有meshの読み取りだけを行う別Viewのsubmitでは失効しない。
`RenderedFrame`のframe所有出力は明示解放まで登録されるため、表示や履歴等のconsumerを投入後に
`release_output`を呼ぶ。CPUメタデータのDropに暗黙の登録解除を結び付けない。
