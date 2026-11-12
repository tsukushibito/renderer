# ADR 0008: M2前の実行資源再利用と性能測定

- 日付: 2026-11-12
- 状態: 採用
- 関連課題: `renderer-3wv`、`renderer-acj`、`renderer-n7l`、`renderer-q7m`

## 背景

M1の往復転送は所有権・同期・内容を確認する基準経路である。
M2の実行基盤には、command資源を転送ごとに作成・破棄せず、
CPU待機を強制せずに複数の処理を投入できる形が必要になる。
基本的な再利用はM2前に整え、容量や高度な配置の最適化は測定に基づいて進める。

## 実行資源の所有と再利用

- privateな`ExecutionPool`がCommandPool・一次CommandBuffer・Fenceの組を遅延作成する。
  呼び出し元が組数の上限を設定する。空き不足は直ちに`CapacityExhausted`として返し、
  待機・再試行は呼び出し元が決める。予約を先に取得し、その後でリソースを渡せるようにする。
- 貸出中の所有者はnative資源とpool状態を保持し、poolの空き資源はVMA・Deviceの所有者を保持する。
  空き資源からpoolへ強参照を戻さず、循環参照を作らない。
  poolやAllocatorの操作用ハンドルを先に破棄しても、貸出中の資源を無効にしない。
- 記録から完了まで、payloadが命令から参照される全リソースを保持する。
  既存の投入・Fence待機・失敗時の保持処理を共用し、公開RHIや任意の生ハンドル操作APIは作らない。
- 未投入キャンセルまたは実際のFence成功待機の後だけ、CommandPoolとFenceをresetする。
  記録された参照先のpayloadを解放する前にcommand状態を解消する。
  reset失敗やDevice停止・lossでは再利用へ戻さない。
- 明示waitの期限切れや完了を証明しない一般エラーは、貸出中の資源を保持して再試行できる。
  [ADR 0005](0005-single-queue-buffer-transfer.md)の有限Drop待機と異常時の保持を継承し、
  Dropでも完了を証明できないときは資源と依存所有者をまとめて保持してDeviceを停止する。
  通常の回収は明示的な完了確認で進め、Drop待機へ依存しない。

## 転送とM2への接続

Uploadだけ・Readbackだけを選べる転送と、複数copyを一つの投入へまとめる経路を設ける。
copyにはbuffer内のoffset・sizeを持たせ、画像全体を隙間のないRGBA列として扱う契約を維持する。
画像の部分領域、行間の余白、複数mip・layerは別の拡張である。

共通のexecutorはHOST依存を無条件に記録しない。Readbackを含むpayloadだけが
CPU読み取りに必要な依存を記録する。M1の往復試験は同じ内容照合を維持する。
M2ではGraphの次用途からstage・access・layoutを導き、copyから描画・Computeを
同じ完了単位へ接続する。transfer-only画像のままで未実装のshader用途を保証しない。

## 共有staging領域の所有

- ユーザーはバッチ間でUpload／Readback bufferを共有し、領域ごとに回収する方式を選択した。
  `TransferPool`はそれぞれの固定容量とnative実行枠の上限を設定する。
  空きの配置・分割・結合はVMA Virtual allocatorを使い、独自の割り当てアルゴリズムは作らない。
- 容量指定は4と`nonCoherentAtomSize`の大きい方の倍数を要求し、領域の位置と予約サイズも
  同じ境界に合わせて隣のバッチとcache atomを共有しない。
  利用可能容量と、alignmentによる予約分を区別する。空き不足は即時エラーで返し、
  内部待機や共有bufferの無制限な増設で隠さない。指定容量はDevice全体のVRAM上限ではない。
  Virtual allocatorの空き不足と、nativeメモリ確保のOOMは別のエラーとして扱う。
- 領域leaseがVMAのvirtual allocationと共有bufferの所有者を保持する。
  未投入キャンセルではleaseを返し、記録・投入から完了まではGPUが使う全領域を保持する。
  native実行枠とUpload領域は成功したFence待機後に返す。
  Readback領域は`TransferBatch::readback`でCPU出力へのコピーに成功するか、
  結果所有者を破棄するまで保持する。読み取り失敗では領域を返さず再試行できる。
- CPUアクセスは所有する領域と、その領域だけに属するcache範囲に限定する。
  GPUが別領域を使う間も、同じbuffer全体をCPUが自由に書き換えられる契約にはしない。
  再取得した領域の過去の内容を新しい要求の初期化済み内容と扱わない。
- Uploadの共有bufferはVMAの常時mapを利用し、既存のcopy-based writeと必要なflushを維持する。
  Readbackはscoped map・invalidate・copyで読み取る。mapped pointerは公開しない。
  完了不明のpending処理はleaseを含むpayloadごと保持し、領域だけを早く再利用しない。

バッチごとに専用bufferを持ち、全体をまとめて返す案も比較した。
その方式は管理が単純だが、サイズの異なるバッチ間で余剰容量を共有しにくい。
今回はVMAによる領域配置を利用し、Renderer側でGPU完了とCPU利用終了を結び付ける方式を採る。
この選択だけで処理速度の向上を保証せず、容量・空き不足・処理時間を測定して評価する。

## 診断と検証設定

Device用debug_utils関数テーブルを一度保持し、QueueもDevice作成時に一度命名する。
転送ごとの詳細な記録・投入・完了は`debug`へ記録し、初期化情報と異常ログは維持する。
上限、作成・再利用・reset回数、空き不足は実行資源管理の状態として確認できるようにする。

既定は`ValidationMode::Required`で、Validation LayerとSyncValを要求する。
Instance・Device・bufferの単独受入試験でも、既存の共通処理で実効設定を確認する。
外部設定でcore validationを無効にしたままの成功を受入結果にしない。

性能測定用に`ValidationMode::Disabled`を明示指定できるようにする。
この場合はValidation Layerとlayer_settingsを要求せず、debug_utilsと診断経路を維持する。
releaseだけで暗黙に検証を外さず、正しさ試験の条件も変えない。
外部loader設定によるLayer注入は別途確認する。

## 性能比較と検証

実GPUの正しさ試験は`#[ignore]`で明示実行し、内容の全byte一致と終了後までの
warning・error・callback内部失敗ゼロを要求する。
再利用では上限数の同時貸出、空き不足、未投入キャンセル、完了後の再取得、
pool先行破棄と依存所有者の保持を確認する。
共有stagingでは複数offsetからのcopy、バッチ間の領域分離、結果保持中のReadback占有、
返却後の再取得と新しい要求の初期化状態を確認する。

性能試験はreleaseと明示的な起動指定を要求し、固定負荷とwarm-upを用いる。
同じcopy命令に対し、native資源を毎回作成するテスト専用の基準経路と再利用経路を比較する。
CPUのwrite・record・submit・wait（回収を含む）・readbackを分け、中央値・p95と資源管理の回数を記録する。
GPU timestampは対応時だけFence成功後に取得し、非対応は未取得と明記する。
基準経路は`#[cfg(test)]`で比較用に残し、通常ビルドへ含めない。

別の比較ではcopy回数と転送総量をそろえ、個別の複数submitと単一バッチのCPU全体時間、
submit回数、staging予約量の最大値を記録する。個別側もすべて投入してから待機する。
このバッチ比較ではGPU時間を測らず、描画FPSや複数フレームの性能を保証しない。

VMAは既にメモリの既定poolとsuballocationを担当している。
採用版には反復map時にnative mappingを保持する仕組みもあるため、
各VMA map/unmap呼び出しを毎回のdriver map/unmapと同一視しない。
常時mapの効果や容量の調整は、既存の再利用も含めた実測から判断する。

実行結果・性能測定・未検証範囲は[再利用基盤の確認結果](../pre-m2-reuse-baseline.md)に記録する。
native reset失敗の実注入やnon-coherentメモリの実機確認は、CPU試験・ソース監査と区別する。

## 一次資料

- [Command資源の再利用と性能測定条件](https://docs.vulkan.org/samples/latest/samples/performance/command_buffer_usage/README.html)
- [vkResetCommandPoolの非pending条件](https://docs.vulkan.org/refpages/latest/refpages/source/vkResetCommandPool.html)
- [vkResetFencesの完了条件](https://docs.vulkan.org/refpages/latest/refpages/source/vkResetFences.html)
- [VMA Virtual allocator](https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/virtual_allocator.html)
- [VMAのメモリmapping](https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/memory_mapping.html)
- [採用VMAのmapping再利用](https://github.com/GPUOpen-LibrariesAndSDKs/VulkanMemoryAllocator/blob/v3.3.0/include/vk_mem_alloc.h#L3359-L3371)
