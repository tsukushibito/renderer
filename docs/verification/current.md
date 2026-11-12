# 現行実装の検証状況

最終確認日: 2026-11-12。実行手順は[開発ガイド](../guides/development.md)、
ツールの使い方は[ツールガイド](../guides/tooling.md)、
保証する契約は[Vulkanの設計判断](../design/vulkan.md)を参照する。
ここには現行実装に対する最新の確認を記録し、過去版の記録はGit履歴で参照する。

## 環境

| 項目 | 確認値 |
|---|---|
| OS / target | Windows x64 / `x86_64-pc-windows-msvc` |
| CPU | Intel Core i5-14600KF |
| Rust / Cargo | 1.99.0 / 1.99.0 |
| GPU / driver | NVIDIA GeForce RTX 3060 / NVIDIA 610.88 |
| Vulkan SDK / Loader | 1.4.341.1 / 1.4.341 |
| Instance / VMA要求API | Vulkan 1.3 |
| Queue | family 0、index 0、Graphics＋Compute 1本 |

## 正しさと品質確認

| 確認 | 結果 |
|---|---|
| fmt、CPUのみ／Vulkanの全target check・Clippy `-D warnings` | 成功 |
| CPUのみの通常試験 | 0件、Vulkan初期化なし |
| Vulkan feature付き通常試験 | CPU 53件成功、明示実行用10件はignored |
| 実Vulkan正しさ試験 | 9件成功、1.21秒 |
| batchの全byte照合 | 2回の生成・終了で計56回、うち順不同の部分転送32回 |
| Validation warning / error、callback内部失敗 | ゼロ |
| Criterionの測定なしの実行確認 | 全24 case成功 |
| 名前付き基準値との比較 | offlineで成功、保存済み基準の内容を保持 |
| GitHub Actions | [Windows／Linuxの品質確認と依存監査の実行結果](https://github.com/tsukushibito/renderer/actions/workflows/ci.yml)を参照 |

実GPU試験ではValidationとSyncValの実効設定を確認した。
半開区間の境界、初期化範囲の穴、範囲統合、各追加順、RAW／WAR／WAW、
read同士、同じcopy内の照合順、global依存後の追跡更新をCPUで確認した。
GPUでは順不同の隙間付きupload、未初期化の隙間の拒否、各範囲のreadbackを照合した。
実GPUの内容照合と、CPUだけで行う範囲管理の試験・性能測定は区別する。

## CPU転送計画の性能測定

Criterion 0.8.2で、8・128・1,024・8,192範囲の順方向・逆方向・固定seedの並べ替え順を測定した。
同じbufferに4-byte writeを16-byte間隔でN件計画し、その後N件のreadを計画する。
40 samples、500 ms warm-up、2秒のmeasurement目標を指定した。
入力生成、初期状態の構築、最後の所有者の破棄は測定外で、
範囲管理の検索・挿入・依存判定と、それに伴う割り当てや追跡集合の破棄を含む。
VMA、byte列のコピー、完全なBatchBuilder、submit、GPU実行は含まない。

以下は現行実装の8,192範囲における推定時間と95%信頼区間である。
中央値、個別転送のp95、描画FPSを表す値ではない。

| 追加順 | 推定時間（ms） | 95%信頼区間（ms） |
|---|---:|---:|
| 順方向 | 16.093 | 13.837–18.291 |
| 逆方向 | 9.710 | 8.105–11.296 |
| 並べ替え順 | 22.294 | 19.202–25.326 |

OSの負荷とthread affinityは固定しておらず、同じソースの実行間にも大きな揺れがあった。
実行内の信頼区間は、この実行間変動を解消しない。
小件数では順序付き集合の管理コストが増える場合があり、安定した改善倍率や損益分岐件数は確定していない。
範囲管理から二次時間の走査・要素移動を除いたことはソース構造で確認している。
通常のCIに壁時計時間の固定合否閾値は設けていない。

## 開発ツール

| ツール | 導入・確認結果 |
|---|---|
| cargo-deny 0.20.2 | 公式SHA-256に一致。Windows／Linux・全feature・開発用／ビルド用依存の4検査が成功 |
| SPIRV-Tools v2026.1 | 固定SDKの`spirv-val`、commit `fbe4f3ad`を確認 |
| shaderc v2026.1 | 一時GLSL ComputeをVulkan 1.3向けSPIR-Vへコンパイルして正常検証 |
| SPIR-V検証スクリプト | 正常入力2件と、不正binary・欠落入力・欠落tool・ディレクトリ入力の4件が期待どおり終了 |
| RenderDoc 1.47 x64 | 公式detached GPG署名が有効。CLI起動、DLLのロード、API 1.7.0取得に成功 |
| RenderDocのVulkan接続 | 子プロセスのmanifest追加パスで、instance／device両方へのcapture layer挿入を確認 |

cargo-denyのadvisory例外はない。Criterionの間接依存による`syn` 2／3の重複warningが1件あり、
理由と扱いを`deny.toml`へ記載している。実VulkanのValidation warning／errorゼロとは別の検査である。
SPIR-Vの正常検証は終了コード0、異常4件は終了コード1だった。
RenderDocでの実GPUキャプチャは未実行である。
portable版のregistry登録は行わず、子プロセス限定の`VK_ADD_IMPLICIT_LAYER_PATH`と
capture有効化指定を使った`vulkaninfo --summary`が終了コード0だった。
通常の利用は[ツールガイド](../guides/tooling.md)のRenderDoc起動方法に従う。

現在のWindows配布物の確認値は次のとおり。

| 配布物 | SHA-256 |
|---|---|
| cargo-deny 0.20.2 x64 MSVC | `975a22143262fd27476d19ee00c7af67978426e40e1dee94eed6bbade1cf87dc` |
| RenderDoc 1.47 x64 ZIP | `b51712e58d40066b34ca30c7869d15f72e4b5c001b2d92232f6783a636d5a3c1` |

RenderDocの署名鍵はfingerprint `1B039DB9A4718A2D699DE031AC612C3120C34695`に照合した。
registryへのVulkan layer登録、global hook、永続PATH、実行ポリシーの変更は行っていない。

## 確認の範囲

この記録の実行確認は上記Windows環境による。ローカルLinux、別CPU・別GPU、
実機non-coherentメモリ、native OOM・Device loss・reset失敗は未確認である。
CPU境界試験やソース監査の成功を、それらの実機確認とは扱わない。
描画・Compute・複数フレーム、完全なBatchBuilderやGPU転送全体の新しい性能値も未確認である。

生ログと配布物はGit対象外の`.tools/`、Criterionの保存値は`target/criterion/`にある。
過去の完全な比較表が必要な場合は、たとえば次のようにGit履歴から参照する。

```sh
git show a032edf:docs/transfer-planning-baseline.md
```
