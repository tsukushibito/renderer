# 依存・SPIR-V・GPU調査ツール

通常のビルドと試験は[開発手順](development.md)を参照する。
以下のコマンドはリポジトリのルートで実行する。
ローカル配布物と確認用ファイルはGit対象外の`.tools/`へ配置する。
導入時の版・配布物の検証・実行結果は[ツールの確認記録](development-tools-baseline.md)に分けて保存する。

## cargo-deny

採用版は**0.20.2**。この環境では公式Windows x64配布物を
`.tools/cargo-deny/bin/cargo-deny.exe`へ配置している。

```powershell
& ./.tools/cargo-deny/bin/cargo-deny.exe --version
& ./.tools/cargo-deny/bin/cargo-deny.exe --locked --color never check
```

別環境では[公式リリース](https://github.com/EmbarkStudios/cargo-deny/releases/tag/0.20.2)の
対応する配布物を取得するか、次のように同じ版をソースから導入する。

```sh
cargo install --locked --version 0.20.2 --root .tools/cargo-deny cargo-deny
```

検査条件は[deny.toml](../deny.toml)で管理する。
Windows MSVCとLinux GNU、全feature、開発用・ビルド用依存を対象に、
advisory、ライセンス、重複・wildcard、取得元を確認する。
CIの`dependencies` jobも同じ設定を使い、公式Action v2.1.1を完全SHA、
実行用Rustを1.99.0へ固定する。

依存ライセンスの許可一覧はApache-2.0、ISC、MIT、Unicode-3.0である。
`licenses.private.ignore`は`publish = false`のworkspace crateを扱うための設定で、
第三者依存の検査除外や、このプロジェクトの配布ライセンス指定ではない。
advisoryの個別除外はない。設定の意味は[公式ライセンス設定](https://embarkstudios.github.io/cargo-deny/checks/licenses/cfg.html)を参照する。

重複版はwarningとして確認する。現在の`syn` 2と3は、Criterionの異なる間接依存が
互換性の異なるmajor版を要求しているため併存する。新しい重複が出たら依存経路と必要性を確認する。
この扱いは、実Vulkan受入試験のValidation warning／errorゼロという条件とは別である。

## SPIR-Vの検証

Vulkan SDK **1.4.341.1**の`glslc`と`spirv-val`を使う。
シェーダーの生成・変換後に、[検証スクリプト](../scripts/validate-spirv.ps1)へ
検証対象のファイルを明示して渡す。

```powershell
& C:/VulkanSDK/1.4.341.1/Bin/glslc.exe --target-env=vulkan1.3 -fshader-stage=compute example.comp -o target/example.spv
& ./scripts/validate-spirv.ps1 -Path target/example.spv -Validator C:/VulkanSDK/1.4.341.1/Bin/spirv-val.exe
```

複数ファイルには`-Path @('target/a.spv', 'target/b.spv')`を指定する。
スクリプトは`--target-env vulkan1.3`で検証し、緩和オプションを使わない。
ファイル・toolの不足、ディレクトリの指定、validatorの失敗は非ゼロで終了する。
ビルド処理から呼ぶ場合は、その終了コードでビルドも失敗させる。

`-Validator`を省略すると、`VULKAN_SDK`、既存のWindows固定SDK、PATHの順に選ぶ。
明示したtoolや`VULKAN_SDK`が無効な場合は、その原因を報告して失敗する。
複数SDKが入っている環境では、上の例のように版の分かるパスを渡す。

現在はshader資産と生成処理がないため、M2のシェーダービルド接続時にこの検証を組み込む。
通常のCPU試験やCPUだけのCriterion測定にVulkan SDKを要求しない。
SDKの構成は[固定版のSPIR-V toolchain](https://vulkan.lunarg.com/doc/view/1.4.341.1/windows/spirv_toolchain.html)を参照する。

## RenderDoc

採用版は**1.47 x64**。この環境では[公式portable ZIP](https://renderdoc.org/stable/1.47/RenderDoc_1.47_64.zip)を
`.tools/renderdoc/RenderDoc_1.47_64/`へ展開している。
版を更新するときは[公式リリース](https://github.com/baldurk/renderdoc/releases/tag/v1.47)と
[公式配布ページ](https://renderdoc.org/builds)から配布物と署名を取得する。

```powershell
& ./.tools/renderdoc/RenderDoc_1.47_64/renderdoccmd.exe version
& ./.tools/renderdoc/RenderDoc_1.47_64/qrenderdoc.exe
```

描画・Compute処理ができたら、qrenderdocから対象実行ファイルを起動して捕捉し、
コマンド列、pipelineやbinding、buffer／imageの内容を確認する。
現在のM1試験はPresentを行わないため、通常のPresent基準の捕捉トリガーでは範囲を決められない。
offscreen試験の捕捉は、M2で必要な範囲へ`StartFrameCapture`／`EndFrameCapture`を接続する。

[公式APIの推奨](https://raw.githubusercontent.com/baldurk/renderdoc/v1.47/docs/in_application_api.rst)に従い、
RenderDocから起動されたときに読み込まれたAPIを利用する。
通常実行や公開Renderer APIへRenderDocを必須依存として持ち込まない。
導入確認ではCLIとDLLのAPI取得までを確認し、実際のGPUキャプチャはまだ行っていない。
