# 依存・SPIR-V・GPU調査ツールの確認結果

- 確認日: 2026-11-12
- 関連課題: `renderer-w5h`
- 利用・再導入手順: [ツールガイド](tooling.md)

## cargo-deny

公式の**cargo-deny 0.20.2 Windows x64 MSVC**配布物を取得し、
`.tools/cargo-deny/bin/cargo-deny.exe`へ配置した。
配布物名は`cargo-deny-0.20.2-x86_64-pc-windows-msvc.tar.gz`、SHA-256は次のとおり。
公式の`.sha256`とGitHub release assetのdigestに一致した。

```text
975a22143262fd27476d19ee00c7af67978426e40e1dee94eed6bbade1cf87dc
```

`--locked --color never check`で`deny.toml`を使い、最新のRustSec DBを取得して
`advisories ok, bans ok, licenses ok, sources ok`、終了コード0を確認した。
advisory例外は追加していない。
warningはCriterionの間接依存による`syn` 2.0.119／3.0.10の重複1件だった。

CIへ公式cargo-deny-action v2.1.1を追加した。固定SHAは
`3c6349835b2b7b196a839186cb8b78e02f7b5f25`、含まれるcargo-denyは0.20.2、
指定Rustは1.99.0である。この導入のローカル確認と、リモートCI実行は別の確認である。

## SPIR-V

既存の固定SDK `C:/VulkanSDK/1.4.341.1`を使用した。

| 確認 | 結果 |
|---|---|
| `spirv-val --version` | SPIRV-Tools v2026.1、commit `fbe4f3ad` |
| `glslc --version` | shaderc v2026.1 |
| 一時GLSL ComputeのVulkan 1.3向けコンパイル | 成功 |
| wrapperのSDK自動選択／validator明示での正常SPIR-V検証 | ともに終了コード0 |
| wrapperへの不正binary、欠落ファイル、欠落tool、ディレクトリ指定 | 4件とも終了コード1 |
| validatorの直接実行で正常／不正／欠落ファイル | 終了コード0／1／1 |

wrapperはWindows PowerShell 5の`-NoLogo -NoProfile -NonInteractive -File`で確認した。
この管理環境ではsandbox内のscript実行が制限されたため、同一コマンドの許可された実行環境で検証した。
実行ポリシーの変更・回避や、永続環境変数の変更は行っていない。
shader資産やM2の生成処理は追加していない。

## RenderDoc

公式**RenderDoc 1.47 x64 portable ZIP**を取得し、
`.tools/renderdoc/RenderDoc_1.47_64`へ展開した。
配布物は101,729,763 bytes、SHA-256は次のとおり。

```text
b51712e58d40066b34ca30c7869d15f72e4b5c001b2d92232f6783a636d5a3c1
```

公式公開鍵のfingerprintを
`1B039DB9A4718A2D699DE031AC612C3120C34695`へ照合し、ZIPのdetached GPG署名の一致を確認した。
署名用の一時keyringはworkspace内に置き、ユーザーの信頼設定は変更していない。

| 確認 | 結果 |
|---|---|
| `renderdoccmd version`、`capture --help` | 終了コード0 |
| 非表示実行の`qrenderdoc --version` | 終了コード0 |
| 配布版commit | `b0e1370309f7d7bf986a73830446ca39d9b8c1fd` |
| `renderdoc.dll`のロードと`RENDERDOC_GetAPI(10700)` | 成功、API 1.7.0 |

実GPUキャプチャは行っていない。現在のoffscreen試験では、M2で捕捉範囲を明示するAPI接続が必要である。
global hook、Vulkan layerの登録、MSI導入、永続PATH変更は行っていない。

## 証拠と範囲

Git対象外のログ・確認データは次に保存した。

- `.tools/cargo-deny-check.log`
- `.tools/spirv-tools-smoke/summary.json`と各caseのログ、生成した一時shader／SPIR-V
- `.tools/renderdoc/install-verification.json`、`signature-verification.log`、`api-verification.json`と各CLIログ

これらはこのWindows環境での導入・起動・validator確認である。
RenderDocでの実キャプチャや、Linuxでのツール実行結果を示すものではない。
CPU・実Vulkan試験、性能測定の条件は[開発手順](development.md)を参照する。
