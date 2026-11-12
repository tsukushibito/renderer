# ADR 0005: 単一キューのbuffer転送とGPU完了

- 日付: 2026-11-12
- 状態: 採用
- 関連課題: `renderer-dj3`

## 背景

[Device基盤](0003-vulkan-device-bootstrap.md)と[VMA buffer](0004-vma-buffer-ownership.md)を
接続し、UploadからGPU上の転送先を経由してReadbackへcopyする。
CPUからのwrite/readだけでは確認できなかった、命令記録から投入・完了までの所有権と
同期、完了後のCPUアクセス、失敗時の後始末を実装する。

## 採用案

- `vulkan` feature配下のprivate実装とし、既存のGraphics＋Computeキュー1本を使う。
  対象はUpload → DeviceTransfer → Readbackのbuffer全体のcopyに限定する。
  汎用encoder、RenderGraph、image、描画、フレーム並列化は追加しない。
- ユーザーの選択により、`submit`と`wait`を分ける。
  記録済みオブジェクトは3つのbufferを値で所有し、submit後は未完了オブジェクトが
  bufferとcommand資源を保持する。記録開始から成功した完了確認まで、呼び出し元へ
  bufferを返さない。CPUアクセスや通常の解放・再利用がGPU実行と競合する経路を設けない。
- 3つのbufferについて、同じAllocator所有者、対応する用途、同じ要求サイズ、
  Upload全体の初期化を記録前に確認する。bufferは単独所有のままで、生ハンドルを公開しない。
  `mem::forget`で未完了オブジェクトが破棄されなくても、bufferとVMA・Device・Instanceを
  保持する参照も残る。Drop実行だけに安全性を依存させない。
- 投入ごとにCommandPool、一次CommandBuffer、Fenceを所有する。
  Fenceは一度の投入と完了を対応づけやすく、今回の経路に十分なため採用する。
  Timeline Semaphoreによるキュー全体の進捗管理は必要になった時点で追加できる。
  独立したCommandPoolを使い、共有するQueueのホスト側操作はDevice所有のロックで直列化する。
- CPU writeの必要なflushを終えてからsubmitする。
  copy間に`TRANSFER_WRITE → TRANSFER_READ`、最後のcopyからCPU readへ
  `TRANSFER_WRITE → HOST_READ`のメモリ依存をSynchronization2で記録する。
  Fenceの完了確認後、必要なinvalidateを既存のbuffer `read`で行う。
  正常経路は対象Fenceだけを待ち、queue／Device全体のidle待ちを使わない。
- 明示`wait`は呼び出し元が待機時間を指定する。成功した場合だけbufferを返し、
  GPU書き込み範囲を初期化済みとする。期限切れは成功と区別し、資源を保持して再試行できる。
  完了を証明しない一般的な待機エラーでも、未完了資源を無条件に解放しない。
- ユーザーの選択により、未完了オブジェクトのDropでは有限時間待って後始末する。
  まず`vkWaitForFences`へ10秒の待機予算を渡す。実装側のtimeout精度による調整があるため、
  壁時計で厳密に10秒以内の終了を保証するものではない。
  完了を確認できないままなら、原因をログに残し、GPUが使い得る資源と依存所有者を
  意図的に保持して通常の破棄を避ける。この異常経路では保持した資源を回収できない。
  Device全体を停止状態にし、新規submit、通常のbuffer作成・CPUアクセスへ戻さない。
- `VK_ERROR_DEVICE_LOST`はDevice全体の終端状態として扱い、自動復旧しない。
  Vulkan仕様のpending／in-use判定に従って子資源の後始末を行うが、
  データが有効になった、転送が成功したとは扱わない。他のbufferも通常利用を拒否する。
  停止状態でも、既存の未完了処理を安全に後始末するための完了確認は行える。
- submitのメモリ不足は、仕様が保証する状態不変の失敗として解放する。
  完了や未投入を証明できない結果を同じ経路へまとめない。
  submit後のログ出力等でunwindしても未完了資源が通常解放されないよう、所有状態を先に確立する。
- `VK_EXT_debug_utils`によるobject名とcommand labelを付け、転送対象、command資源、
  Fenceを診断へ対応づける。エラーには操作名・元の`vk::Result`を残す。
  投入、完了待ち、期限切れ、停止、回収できず保持した資源をログに記録する。
- Instanceで`VK_EXT_layer_settings`を必須拡張として照会・有効化し、
  `validate_sync=true`を指定する。core validationを維持してSynchronization Validationを追加するよう要求する。
  globalと有効なvalidation layerの提供拡張を調べ、不足したまま試験を成功させない。
  API指定より環境変数・設定ファイルが優先されるため、要求ログだけで実際の有効状態を保証しない。
  実Vulkan受入時は外部設定と、レイヤー起動時のINFOメッセージの有効状態を確認する。

## 比較した案と影響

転送と完了待ちを一つの同期関数にすれば今回のsmokeは簡単になるが、
ユーザーは投入後にCPU処理を挟める形を選択した。今回の試験では直後に明示waitする。
これは将来の通常フレームで、毎回すべてを待つという規約ではない。

未完了オブジェクトのDropを待機なしにする場合は、キュー側に保持と回収処理が必要になる。
今回はユーザーの選択により、明示waitを基本とし、Dropにも安全な後始末を持たせる。
Dropは待機し得る。将来のフレーム寿命管理では、正常時にDrop待機へ依存しない構成へ進める。

## 検証と見直し

CPU試験では、入力契約、投入・待機の結果による所有状態、成功時のbuffer返却、
期限切れや一般エラーの保持、Device loss時の停止と解放を確認する。
置き換えは必要な外部境界に限定し、VulkanやVMA全体を模倣しない。

実GPU試験は`#[ignore]`で明示実行し、既知パターンの全byte一致と、
パターンを変えた完了後のbuffer再利用、未完了オブジェクトのDrop、終了までを確認する。
最終解放後まで診断を集計し、validation warning／errorとcallback内部失敗はゼロを要求する。
例外は規約で定めた確認済みの誤検出だけとし、現在の例外はない。

Synchronization Validationはhost memory accessを追跡しないため、実GPU試験だけを
CPUアクセス禁止や失敗時の寿命の証拠にはしない。正常な実GPUへ投入した後に
Device lossを偽装して解放する試験も行わない。CPU境界の失敗注入と実driverの異常は区別する。
non-coherentメモリ、実driverのOOMやDevice loss等も、実施した範囲を明記する。

実行結果と未検証項目は[buffer転送・完了待ちの確認結果](../m1-transfer-baseline.md)に記録する。
image、フレームをまたぐ再利用、複数キュー、一般的な遅延破棄を追加する際に見直す。

## 一次資料

- [vkQueueSubmit2の投入・状態・失敗時の保証](https://docs.vulkan.org/refpages/latest/refpages/source/vkQueueSubmit2.html)
- [vkWaitForFencesの完了待ち・timeout・Device loss](https://docs.vulkan.org/refpages/latest/refpages/source/vkWaitForFences.html)
- [Vulkan仕様のLost Device](https://docs.vulkan.org/spec/latest/chapters/devsandqueues.html#devsandqueues-lost-device)
- [Khronosの同期例: upload、copy間の依存、CPU readback](https://docs.vulkan.org/guide/latest/synchronization_examples.html)
- [LunarGのSynchronization Validation設定と制限](https://vulkan.lunarg.com/doc/view/latest/windows/synchronization_usage.html)
- [LunarGのレイヤー設定と優先順位](https://vulkan.lunarg.com/doc/view/latest/windows/layer_configuration.html)
- [Rustのmem::forgetと安全性](https://doc.rust-lang.org/std/mem/fn.forget.html)
