# ADR 0004: VMA bufferの所有権とCPUアクセス

- 日付: 2026-11-12
- 状態: 採用
- 関連課題: `renderer-yr6`、後続 `renderer-dj3`

## 背景

[Device基盤](0003-vulkan-device-bootstrap.md)の上に、VMAによるbuffer作成、
CPUアクセス、解放を追加する。GPUへの転送命令を実装する前に、Allocationの所有権、
アクセス範囲、mapとキャッシュ処理、失敗時の後始末を確認する。

## 採用案

- `vulkan` feature配下のprivate実装とする。VMAにはInstanceの要求と同じ
  Vulkan 1.3を指定し、未使用のAllocator拡張フラグは有効にしない。
  VMAのメモリタイプ選択とサブアロケーションを利用し、独自Allocatorやpoolは作らない。
- Allocatorの所有者はVMA Allocatorと`VulkanDevice`を値で保持し、
  VMA Allocatorを先に、DeviceとそのInstanceを後に破棄する。
  Allocatorのハンドルと各bufferは、この所有者を`Arc`で保持する。
  bufferとAllocationの組は単独所有し、CloneやCopyを提供しない。
  作成成功直後から所有型へ収め、後続の情報照会やログ出力で中断しても解放する。
- この共有はAllocationが必要とする所有者の寿命を保つために限定する。
  Allocatorのハンドルが先に破棄されても、bufferが残る間はVMAとDeviceを維持する。
  `mem::forget(buffer)`でも所有者への参照が残り、未解放Allocationの下で
  VMAだけを先に破棄しない。明示的にリークしたリソースの回収は保証しない。
- 用途は次の3種類に限定する。

  | 用途 | Vulkan usage | VMAの選択 | CPUアクセス |
  |---|---|---|---|
  | Upload | transfer source | AutoPreferHost、HOST_ACCESS_SEQUENTIAL_WRITE | writeのみ |
  | DeviceTransfer | transfer source / destination | AutoPreferDevice | 不可 |
  | Readback | transfer destination | AutoPreferHost、HOST_ACCESS_RANDOM | read / write |

  ReadbackのCPU writeは、GPU転送前の初期化や今回のCPUアクセス試験にも使う。
  実際のメモリがHOST_VISIBLEでも、DeviceTransferのCPUアクセスは許可しない。
  host用途で非HOST_VISIBLEメモリへの自動切り替えは行わない。
- ユーザーの選択により、今回はバイト列を渡すwrite/readだけを実装する。
  mapped pointer、slice、VMA Allocation、生のbuffer操作を外へ公開しない。
  直接マップ領域を操作するAPIやpersistent mappingは、必要になった時点で追加する。
- buffer sizeのゼロを拒否する。CPUアクセスでは要求したbuffer sizeを基準に
  offsetと長さ、加算とポインター操作に必要な変換を検証する。
  VMA Allocationの余白をアクセス可能なbuffer領域とみなさない。
  有効な範囲のゼロ長アクセスは何もせず成功する。
- 確保時の全面ゼロ埋めは行わない。成功したwriteの範囲を記録し、隣接・重複を統合する。
  readは、その範囲全体の内容が確立済みの場合だけ許可する。
  書き込み範囲の間にある未初期化の穴は読み出せない。
- map成功直後にguardで管理し、通常終了、キャッシュ操作の失敗、unwindでもunmapする。
  write後は必要なflush、read前は必要なinvalidateを行う。
  HOST_COHERENTではこれらのキャッシュ操作を省略する。
  VMAへ渡すoffsetはAllocation先頭からの相対値であり、Allocationの配置offsetを加算しない。
  nonCoherentAtomSizeへの丸めはVMAへ任せる。
- エラーは操作、buffer用途・範囲などの文脈と、元の`vk::Result`を保持する。
  診断にはbufferの用途・要求size・実際のメモリタイプとproperty flagsを残す。
  write失敗時の内容のロールバックは保証しない。write中のmapやキャッシュ操作が失敗した
  場合は初期化済み範囲の記録を解除し、成功したwriteで内容を再び確立してからreadを許可する。

## 比較した案と影響

Allocatorをbufferが借用する案は通常の破棄順を制約できるが、
bufferを`mem::forget`した後もVMAのAllocationは残る。
所有者への参照もbufferに持たせ、Allocatorだけが先に破棄される経路を避ける。
Instance単体を広く共有するAPIや、汎用Backend traitを導入するものではない。

mapped viewはコピーを省く選択肢になるが、今回はCPUアクセスとmap解除の責任を
内部へ閉じ込める。初期化範囲を記録することで全面ゼロ埋めを避けつつ、
書いていない領域を通常のreadで読み出すことも防ぐ。

`Arc`はGPU完了を保証しない。今回はGPUへコマンドを投入せず、
bufferの生ハンドルを使う投入経路も提供しない。
後続の`renderer-dj3`で、命令記録から完了までのリソース保持、CPUアクセスの制限、
GPUが書いた範囲の内容確立、submit/wait失敗時の状態を実装する。
完了前に通常の破棄や再利用へ戻してよいという契約にはしない。

## 検証と見直し

CPU試験では、範囲と初期化状態、アクセス用途、キャッシュ処理の順序、
mapやキャッシュ操作の失敗時の後始末を確認する。
試験用の置き換えは必要なメモリアクセス境界に限定し、VMAやVulkan全体を模倣しない。

実Vulkan試験は`#[ignore]`で明示実行し、3用途の作成・CPUアクセス・解放を反復する。
全bufferとAllocator、Device、Instanceの解放後までValidationを集計し、
warning、error、callback内部失敗ゼロを要求する。
実メモリのcoherent / non-coherentを記録し、未実行の種別を検証済みと扱わない。
GPU copy、GPUからのreadback、同期命令、image、描画の成功は今回の結果に含めない。

実行結果と未検証の範囲は[bufferの確認結果](../m1-buffer-baseline.md)に記録する。
GPU投入、永続リソース、直接mapped view、複数キュー、imageやaliasingを追加する際に見直す。

## 一次資料

- [vk-mem 0.5.0 Allocatorの寿命・map・キャッシュ操作・解放](https://docs.rs/vk-mem/0.5.0/vk_mem/struct.Allocator.html)
- [vk-mem 0.5.0 AllocatorCreateInfo](https://docs.rs/vk-mem/0.5.0/vk_mem/struct.AllocatorCreateInfo.html)
- [VMAのメモリマッピングとキャッシュ処理](https://gpuopen-librariesandsdks.github.io/VulkanMemoryAllocator/html/memory_mapping.html)
- [Rustのmem::forgetと安全性](https://doc.rust-lang.org/std/mem/fn.forget.html)
