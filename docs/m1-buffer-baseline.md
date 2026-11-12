# M1: VMA bufferの確認結果

確認日: 2026-11-12。対象課題: `renderer-yr6`。
設計判断は[ADR 0004](adr/0004-vma-buffer-ownership.md)、
実行手順は[開発環境](development.md)を参照する。
M1全体ではなく、VMA bufferの所有・CPUアクセス・解放までの確認である。

## 実装した範囲

`vulkan/memory.rs`にVMA AllocatorとDeviceの所有者を、`vulkan/buffer.rs`に
3用途のbuffer作成と単独所有、範囲を検証したwrite/readを追加した。
`vulkan/buffer/host_access.rs`がmap/unmapと必要なflush/invalidateを扱う。
ユーザーの選択により、今回はバイト列をコピーするAPIだけを用意し、直接mapped viewは設けていない。

各bufferはAllocatorとDeviceの所有者への`Arc`を保持する。
Allocatorの操作用ハンドルを先に破棄してもbufferは存続し、
最後の所有参照を解放するとVMA、Device、Instanceの順に破棄する。
未初期化の範囲はreadを拒否し、成功したwriteの範囲だけを統合して記録する。
GPUコマンド投入、GPU copy、image、描画、公開リソースAPIは追加していない。

## 実行環境とメモリ選択

| 項目 | 今回の確認 |
|---|---|
| OS / target | Windows x64 / `x86_64-pc-windows-msvc` |
| Rust / Cargo | rustc 1.99.0 (`b940084d7`) / cargo 1.99.0 (`5f94df478`) |
| ash / vk-mem | 0.38.0+1.3.281 / 0.5.0。lockfileの変更なし |
| 調査SDK | `C:/VulkanSDK/1.4.341.1/Bin/vulkaninfoSDK.exe` |
| GPU / driver | NVIDIA GeForce RTX 3060、DISCRETE_GPU / NVIDIA 610.88 |
| Instance要求 / loader / Physical Device API | 1.3.0 / 1.4.341 / variant 0、1.4.341 |
| VMAへ指定したAPI | 1.3 |
| 検証 | `VK_LAYER_KHRONOS_validation`（報告API 1.4.341）、`VK_EXT_debug_utils`必須 |

全4反復で、各bufferの要求sizeは257 byte、VMAの報告Allocation sizeは272 byteだった。
アクセスの上限は要求した257 byteとし、Allocationの余白は使わない。

| 用途 | 選択されたmemory type | 実際のproperty flags |
|---|---|---|
| Upload | 3 | HOST_VISIBLE、HOST_COHERENT |
| DeviceTransfer | 1 | DEVICE_LOCAL |
| Readback | 4 | HOST_VISIBLE、HOST_COHERENT、HOST_CACHED |

SDK調査ではmemory typeは6種類で、HOST_VISIBLEを持つtype 3、4、5はすべて
HOST_COHERENTだった。non-coherentメモリの実機経路は、この環境では利用できなかった。

## 実行した確認

| 確認 | 結果 |
|---|---|
| `cargo fmt --all -- --check` | 成功 |
| CPU経路のworkspace / all-targetsのcheck、clippy (`-D warnings`) | 成功 |
| CPU経路の通常test | 成功、テスト0件。Vulkanを初期化しない |
| Vulkan feature付きworkspace / all-targetsのcheck、clippy (`-D warnings`) | 成功 |
| Vulkan feature付きの通常test | CPUテスト26件成功、実Vulkan試験4件はignored |
| 明示実行 `vulkan_buffer_smoke` | 成功。3用途の作成・CPUアクセス・解放を4回反復 |
| 既存の実Vulkan試験 | Instance正常系、Instance部分初期化失敗、Deviceの3件も成功。各4回反復 |
| 全実Vulkan試験の終了後集計 | 全反復でValidation warning 0、error 0、callback内部失敗0。例外登録なし |

今回まとめて実行したコマンド:

```sh
cargo test --locked -p renderer --no-default-features --features vulkan backends::vulkan:: -- --ignored --nocapture
```

buffer試験だけを再現するコマンド:

```sh
cargo test --locked -p renderer --no-default-features --features vulkan backends::vulkan::buffer::tests::vulkan_buffer_ -- --ignored --nocapture
```

追加したCPU試験は6件で、範囲とoverflow、初期化範囲の穴・隣接・重複の統合、
用途ごとのアクセス制限、実際の作成要求と作成失敗の元原因保持を確認した。
同じwrite/read処理を通す小さなメモリアクセス境界で、coherent / non-coherentの
キャッシュ操作の違い、コピーとの順序、map / flush / invalidate失敗時のunmapを確認した。
VMAのatom丸めやVulkan全体を模した試験実装は作っていない。

実GPUではReadbackに既知パターンを部分的にCPUから書き込み、未初期化の穴を拒否した後、
穴を埋めて257 byteすべての一致を確認した。UploadへのCPU write、用途と範囲の拒否、
Allocatorの操作用ハンドルを先に破棄した後のbuffer継続利用も確認した。
最後のbuffer解放後に所有者へのWeak参照が消滅し、Instanceまでの終了後に診断を集計した。
拒否される入力も意図的に試すため、renderer自身の入力エラーログは出力される。
これはValidationのwarning/errorとは区別している。

## 未検証の範囲と次の作業

non-coherent向けの手順はCPU試験で確認しており、実機でのflush/invalidate動作確認は未実施である。
作成失敗とmap/cache失敗のCPU試験は境界への注入であり、実ドライバーのOOMやmap失敗の再現ではない。
Linuxの実行、リモートCI、CPU型fallback、複数GPU環境も今回の確認には含めない。
Windows / Linuxの既存CI構成は維持した。

今回の全byte一致はCPUで書いた内容のCPU読み出しであり、GPUからのreadbackではない。
GPU同期、Synchronization Validationの明示設定、GPU完了管理、転送や描画の性能は検証していない。
次の`renderer-dj3`で単一キューのcopy、submit、完了待ち、GPU転送後のreadback照合を実装する。
M1全体の完了には、その後に最小限のimage経路も必要である。
