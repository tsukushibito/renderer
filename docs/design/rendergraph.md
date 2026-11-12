# RenderGraphの契約

Graphの宣言・解析はGPUとウィンドウに依存せず、通常のCPU構成で利用する。
Vulkan実行は`vulkan` featureで追加し、公開APIへash・VMAの型を出さない。
全体方針は[実装方針](implementation-policy.md)、native資源と完了管理は
[Vulkanの設計](vulkan.md)、実行済みの確認は[検証状況](../verification/current.md)を参照する。

## 対応範囲と処理の境界

最初の経路は、単一Graphics＋Computeキュー、物理aliasingなし、
2D画像のmip・layer・sample各1を対象とする。
bufferは半開byte範囲を指定し、未対応の画像subresource指定は拒否する。
論理ハンドルと物理資源の識別、内容の有効性と画像layout、
予定されたGPU状態と実際の完了を別々に管理する。

通常のパスは、管理下のcopy・Compute・indexed mesh・最終合成という限定した操作を宣言する。
任意の生CommandBuffer、外部SPIR-V、ネイティブ外部ハンドルは受け入れない。
これにより、実行する操作と宣言した用途・範囲・bindingを対応づける。
汎用RHIや未信頼shaderのサンドボックスを提供する契約ではない。
依存するCompute処理は別ノードとし、パス間の同期を実行基盤が記録する。

## producer・範囲・Attachment

資源は可変で、readは宣言順で直前までに書かれた内容を使う。
bufferの異なるbyte範囲には異なるproducerがあり得る。
一つのパスのreadをすべて検査してからwriteを反映するため、
`read_write`宣言自体では未初期化内容を初期化できない。
forwardの明示依存は実行順序を制約するが、readが読むversionを変更しない。
明示依存の先は意味的な前提として残し、循環はdeadな部分も含めて拒否する。
ハンドルはappend-onlyなGraph内だけで有効で、別Graphでの利用を拒否する。

宣言の範囲・用途・初期化は不要パス除去の前に検査する。
deadなreadも未初期化ならエラーになり、不要な処理で入力ミスを隠さない。
解析専用の`declare_pass`と実際の操作builderを分け、実行時にliveな解析専用パスを拒否する。
任意の実行callbackはまだ提供しないので、Graph構築や除去をCPUのゲーム更新の代わりにしない。

upload本文は不変の`Arc<[u8]>`で共有し、compileで全量を複製しない。
既存の`upload_buffer`／`upload_image`はVec入力を受け取り、共有表現へ一度変換する。
`upload_buffer_shared`／`upload_image_shared`へ同じArcを渡せば、Graphの再構築でも入力を共有できる。
compiled planには除去したパスの名前・アクセス等の診断情報を残し、実行本文は保持しない。
dumpは要求時に一度生成する。

Attachmentは画像全域をrender areaとする。`Load`は既存内容を要求し、
`Clear`は全域を初期化する。loadの`Discard`は部分的なmesh描画だけで全域を
確立したとみなさない。storeの`Discard`は後続の内容を無効にする。
`Load`／`Clear`と`Store`の組だけが、meshの外側も含めた有効内容を引き継げる。
exportする資源の全域にも有効な内容を要求する。

メッシュは7個のf32（xyz、linear rgba）、stride 28 byte、u32の三角形indexを使う。
CPUのRust構造体を直接キャストせず、各値を明示的にbyteへ変換する。
runtimeはupload・copyから既知のindex内容を追跡し、index値が頂点数未満であることを確認する。
Deviceの実際の`maxDrawIndexedIndexValue`も確認し、確認できない内容や範囲外indexを
native命令の記録前に拒否する。featureを有効化していないことから上限を固定値へ置き換えない。

内容追跡はupload入力の共有sliceを参照し、GPU copyではbyte列をCPUで複製しない。
部分書き換え・copyの未知の穴を保持し、sliceを跨ぐindexも一時bufferを作らず検査する。
現在のindex用途とそのcopy元、import／exportしたbufferに必要な内容だけを追跡する。
import／exportしたbufferは後のGraphでindexに使えるため、元の入力を共有したまま保持する。
この保持は新しいCPUミラーの作成とは異なるが、小さなsliceが元の入力全体を保持する場合はある。

## 論理的な依存と実行順

内容のproducerを特定するdataflowと、同じ物理資源のRAW・WAR・WAWによる順序制約は区別する。
不要パス除去はexport・readback・外部に観測される書き込み・明示的な副作用から
必要なproducerを辿る。上書き済みwriterや不要なreaderを、順序制約だけで残さない。
nativeのstage・access・layoutは、残った実際の操作から具体化する。
トポロジカル順序だけではVulkanのメモリ依存は成立しない。

順序依存は資源の区間ごとのwriterと未処理readerから作り、全先行パスとの総当たりを行わない。
全宣言の循環を確認してから、残ったパスだけで順序依存を組み直す。
除去された途中のwriter／readerが、必要なパス同士の順序を消さないためである。

## GPU状態と所有

同じruntime資源のimportは、物理資源と状態の同一性を維持する。
exportハンドルの取得はCPUで内容を読めることを意味しない。
単一キューへproducer Graph Aとconsumer Graph Bを投入する場合、
Bは受理済みのAの最終状態を使い、両者の間に必要な依存を記録する。
CPU readbackと再利用には実際の正常完了が必要である。

各資源は直近のread範囲だけでなく、後続用途へ可視化する必要のあるwriterのscopeも引き継ぐ。
copyの書き込みを一度copyで読んだだけでは、vertex／index入力へも可視になったとはしない。
bufferのnative依存は現段階では全体を対象に保守的に記録し、
Graphのbyte範囲によるproducer解析と、native barrierの粒度を区別する。

`VulkanRuntime::submit`は実行枠を予約してから、一つのGraphを記録・投入する。
`Submission::export_buffer`／`export_image`は受理済み状態のsnapshotを返す。
これを次のGraphでimportして投入でき、AをCPUで待つ必要はない。
snapshotにはowner・世代・revisionがあり、別runtime、削除済み、更新前のsnapshotを拒否する。
更新後は`refresh_buffer`／`refresh_image`で取得し直し、Graphを再構築・compileする。
実行時にもcanonicalな状態と照合するため、compile後の変更を取り逃さない。

exportした資源はruntimeへ登録され、明示的な`delete`で登録を解除する。
CPUハンドルをdropするだけでは登録を解除しない。
未登録資源は保持参照がなくなってから再利用でき、同じ作成条件ならallocationとViewを保持する。
異なる作成条件の資源が必要なら、空いている未登録entryを置き換えられる。
resource容量は保持する物理entry数の上限であり、DeviceのVRAM budgetではない。
exportを保持し続けた場合も、容量不足を返し、内部で待機・無制限増設はしない。

`Submission::wait`のtimeoutは`None`で、同じsubmissionを再度待てる。
正常完了後は`SubmissionResult::readback`へreadbackパスのハンドルを渡す。
CPUコピー成功時にそのstaging領域を解放し、二重消費と別Graphのハンドルを拒否する。
読み取り前の結果を保持していても、完了した実行枠は先に再利用できる。

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

## 同期と一般推奨の適用

[ベストプラクティス](../guides/vulkan-best-practices.md#synchronization)に従い、
Copy、Compute storage、fragment sampling、vertex/index入力、color/depth attachment、
readbackのHOST_READを実際の用途へ対応づける。
Dynamic Renderingのパス間barrierはrendering scopeの外へ置く。
readbackがない処理にはHOST_READ依存を追加しない。

単一キュー、保守的な同期、通常のdescriptor set、固定shaderは正しさを確認する基準である。
vendor依存の高速化や高度なパス統合は採用しておらず、性能改善を主張しない。
WSI、複数キュー、物理aliasing、一般的なフレーム履歴とエンジン資源APIは後続の対象とする。
