# M2前の再利用基盤・共有stagingの確認結果

- 確認日: 2026-11-12
- 関連課題: `renderer-3wv`、`renderer-acj`、`renderer-n7l`、`renderer-q7m`
- 判断と契約: [ADR 0008](adr/0008-pre-m2-execution-reuse.md)
- 再現手順: [開発手順](development.md)

## 実装範囲

CommandPool・一次CommandBuffer・Fenceを上限付きで再利用する。
空き不足は待たずに返し、呼び出し元が明示wait・再試行を選ぶ。
native資源を再利用できるのは、未投入キャンセルまたは実際のFence完了後である。
通常ビルドは再利用経路を使い、毎回作成する比較用経路はテストにだけ残す。

ユーザーが選択した共有方式として、Upload／Readback各1本のbuffer内の領域を
複数バッチで分けて使う。配置と解放にはVMA Virtual allocatorを用いる。
要求byte数と、copy・cache atom境界へ整列した予約byte数を区別する。
Uploadは常時mapとcopy-based write、Readbackはinvalidateとcopy-based readを使う。
mapped pointerは呼び出し元へ渡さない。

複数のbuffer copy、画像全体のpacked RGBA8 upload／readbackを一つの投入へまとめる。
Upload領域はFence成功後、Readback領域はCPUコピー成功または結果の破棄後に返す。
未読結果の保持中も、native実行枠は先に再利用できる。
完了不明時は領域を含む所有者を保持し、既存の有限Drop待機・Device停止方針を維持する。

共通executorの無条件HOST依存を除き、readbackがある場合だけ記録する。
bufferの依存は重複範囲と読み書きに応じて入れ、独立したcopyを毎回直列化しない。
Deviceのdebug_utils関数テーブルは一度保持し、Queueも一度命名する。
正常な転送ごとの詳細はdebugログへ移し、初期化・異常時のログを維持する。

## 環境

| 項目 | 確認値 |
|---|---|
| OS / target | Windows x64 / `x86_64-pc-windows-msvc` |
| Rust / Cargo | 1.99.0 / 1.99.0 |
| Vulkan SDK / Loader | 1.4.341.1 / 1.4.341 |
| Instance / VMA要求API | Vulkan 1.3 |
| GPU | NVIDIA GeForce RTX 3060、DISCRETE_GPU |
| vendor / device | `0x10de` / `0x2487` |
| driver | NVIDIA 610.88（vulkaninfo表示 `610.88.0.0`） |
| Validation Layer API | 1.4.341 |
| Queue | family 0、index 0、Graphics＋Compute 1本 |
| GPU timestamp | valid bits 64、period 1 ns |

正しさ試験ではValidationとSyncValを要求し、実効起動情報も確認した。
性能試験は`ValidationMode::Disabled`を明示し、release・ログ出力なしで実行した。
既知のLayer／Loader上書き環境変数9項目が未設定で、作業ディレクトリの
`vk_layer_settings.txt`と既知のVulkan Configurator設定2ファイルが存在しないことを記録した。
起動メッセージがないことだけで、あらゆる外部設定の不在を証明したとは扱わない。

## 正しさの確認

| 確認 | 結果 |
|---|---|
| fmt、CPUのみ／Vulkanの全target check・Clippy `-D warnings` | 成功 |
| CPUのみの通常試験 | 0件、Vulkan初期化なし |
| Vulkan feature付き通常試験 | CPU 47件成功、明示実行用10件はignored |
| 実Vulkan正しさ試験 `::vulkan_ -- --ignored` | 9件成功、1.09秒 |
| Validation warning / error、callback内部失敗 | ゼロ |
| Instance／Device／buffer単独試験でcore validationを外部無効化 | 3件とも受入を拒否（期待した失敗） |
| batch試験でSyncValを外部無効化 | 受入を拒否（期待した失敗） |
| 性能試験へValidation Layerを外部注入 | 測定を拒否（期待した失敗） |

新しい実行枠の試験では予約と記録のキャンセル、2件の未回収処理、上限不足、
完了後の再利用、pool／Allocator先行破棄を確認した。
共有バッチ試験は2回の生成・終了で24回の全byte照合に成功した。
複数offset、GPU内copy、upload-only／readback-only、部分初期化範囲、
同一バッチ内のbuffer／imageの上書きと各時点のreadbackを含む。
有効な領域を持つBuilder／記録済みオブジェクトのキャンセル回収、
未読結果による容量保持、再取得したReadbackの未初期化拒否も確認した。

CPU試験は領域境界・ID・同期条件・VMA予約の整列と回収など、この実装の契約を確認する。
native作成やresetの失敗を実ドライバーへ注入した結果ではない。
未知の失敗時の保持は既存の所有状態試験とコードレビューで確認した。

## 性能測定

各比較は20回のwarm-up後に200回測定し、実行順を交互にした。
パターン生成と全byte照合は計測区間から外した。
以下はこの実機での一回の測定であり、一般的なFPS改善率は示さない。

### native実行資源の再利用

同一ビルドで1 MiBのUpload → DeviceTransfer → Readbackを比較した。
bufferは両経路とも再利用し、native実行資源の毎回作成と再利用を切り替えた。
CPUのwait区間には完了後のreset／破棄による回収も含む。

| 区間 | 毎回作成 中央値 / p95（µs） | 再利用 中央値 / p95（µs） |
|---|---:|---:|
| write | 23.4 / 97.4 | 24.5 / 97.6 |
| record | 16.4 / 55.7 | 6.0 / 19.7 |
| submit | 12.6 / 45.0 | 13.2 / 47.4 |
| wait＋回収 | 132.5 / 181.1 | 127.0 / 167.4 |
| readback | 59.4 / 151.7 | 62.7 / 154.7 |
| CPU区間合計 | 248.5 / 529.5 | 235.7 / 475.5 |
| GPU命令 | 89.632 / 93.856 | 89.888 / 94.432 |

warm-up込みで各経路220回投入し、全byte照合は計440回成功した。
再利用側のnative作成は1回、再利用219回、reset220回だった。
両経路ともGPU copy総量は461,373,440 bytesである。
GPU時間の短縮を示す結果ではない。CPUのp95にも揺れがあり、単一機材の基準値として扱う。

### 複数copyのバッチ化

8件 × 64 KiBのuploadとreadback、計16copyを比較した。
個別側も8件すべてをsubmitしてから待ち、一件ずつのCPU待機を入れていない。
target bufferは事前作成し、反復間で再利用した。
CPU全体時間にはbuilder・領域管理・投入・完了・CPU readbackが含まれる。
この比較ではGPU時間は測定していない。

| 指標 | 8件を別投入 | 1バッチ |
|---|---:|---:|
| CPU全体 中央値（µs） | 177.2 | 148.0 |
| CPU全体 p95（µs） | 342.8 | 194.0 |
| submit回数（warm-up込み） | 1,760 | 220 |
| native実行資源作成 | 8 | 1 |
| 再利用 / reset | 1,752 / 1,760 | 219 / 220 |

各経路のcopy総量は230,686,720 bytes、共有staging bufferの作成は各2本だった。
Upload／Readbackの予約量の最大値はそれぞれ524,288 bytesで、設定容量内に収まった。
全byte照合は計3,520回成功し、性能試験全体の最終診断もゼロだった。
これは複数の実装処理を含む比較であり、submit単独の改善量や描画性能とは扱わない。

## 証拠と制限

この作業環境のGit対象外ログは`.tools/pre-m2-final-*.log`、
`.tools/pre-m2-transfer-performance.log`、`.tools/pre-m2-performance-environment.log`、
`.tools/pre-m2-performance-injected-validation.log`に保存した。
生ログの共有を前提にせず、再現手順と結果は本書に記録する。

ローカルのLinux実行、別GPU・CPU fallback、実機non-coherentメモリ、
実ドライバーのOOM・Device loss・長時間stall・native reset失敗は未検証である。
共有領域のcache atom隔離はコード・CPU境界試験で確認したが、
coherentメモリの成功をnon-coherent実機の成功とは扱わない。

本実装の既存GPUアクセスはCOPYに限定される。
描画・Computeのusage、ImageView、pipeline、次用途のstage/access/layout計画はM2で接続する。
複数フレームのGraph寿命・遅延破棄・Swapchainの実行結果ではない。
設定したstaging容量はVkBufferの要求byte容量であり、VMAの物理block保持や
Device全体のVRAM使用量を固定上限へ抑える保証ではない。
