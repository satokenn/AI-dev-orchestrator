# Rust コード品質・テスト方針

この文書は、AI Dev Orchestrator の Rust コードに適用する初期の品質基準とテスト方針を定義します。複数の開発者や AI エージェントが同じ基準で変更を作成し、CI が機械的に合否を判定できることを目的とします。

本体開発を始めるための最小基準に限定し、追加の Clippy lint 群、対応プラットフォーム、性能基準などは必要性が明確になった時点で見直します。

## 必須の検証

Rust workspace の作成後は、次のコマンドを Pull Request の必須検証として扱います。

| 目的 | コマンド | 合格条件 |
| --- | --- | --- |
| format | `cargo fmt --all -- --check` | 差分を生成せず終了コードが 0 になる |
| lint | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | rustc と Clippy の warning がなく、終了コードが 0 になる |
| test | `cargo test --workspace --all-features` | 対象となる Unit / Integration / Doc Test が全て成功する |

`cargo build` は初期の独立した必須項目にはしません。Clippy とテストでもコンパイルを行うためです。リリース成果物固有のbuild検証が必要になった場合は、対象とコマンドを後続Issueで追加します。

## formatter

- CI では `cargo fmt --all -- --check` を実行し、未整形のコードを失敗として扱う。
- 開発者やAIエージェントは、修正時に `cargo fmt --all` を使用できる。
- rustfmtの個別設定は、デフォルトから変更する具体的な必要性が生じるまで追加しない。

## lintとwarning

- CI の Clippy では `-D warnings` を指定し、rustcとClippyのwarningをエラーとして扱う。
- `--workspace --all-targets --all-features`を指定し、全workspace member、test、example、benchmarkを含む全targetと、全featureを検査する。
- Clippyは、プロジェクトのコンパイルに使用するRust toolchainと同じtoolchainで実行する。
- `clippy::pedantic`や`clippy::nursery`などの追加lint群は、初期の必須基準にしない。
- lintを抑制する場合は、対象を必要最小限に限定し、その場に理由を記載する。プロジェクト全体の抑制はDesign / RFC Issueで合意する。
- 将来、同時に有効化できないfeatureが導入された場合は、`--all-features`を無条件に外さず、CIのfeature matrixをDesign / RFC Issueで定義する。

## テスト追加の基準

- 振る舞いを追加・変更する場合は、その変更を直接確認できるテストを追加または更新する。
- 不具合修正では、修正前に失敗し、修正後に成功する回帰テストを追加する。
- 既存テストで変更を十分に検証できる場合や、文書だけの変更などテスト対象がない場合は、新しいテストを必須としない。
- テストを追加しない判断が自明でない場合は、Pull Requestに理由を記載する。
- テストはネットワーク、時刻、実行順序などの外部状態へ不必要に依存させず、同じ入力に対して再現可能にする。

## テスト種別の境界

### Unit Test

- 単一moduleや小さなロジックを分離して検証する。
- private interfaceを含め、実装対象と同じsource file内の`#[cfg(test)]` moduleへ配置する。
- 外部process、network、実Providerには接続しない。

### Integration Test

- crateのpublic interfaceや、複数componentを組み合わせた振る舞いを検証する。
- Cargoの慣例に従い`tests/`へ配置する。
- filesystemやprocessが必要な場合は、隔離した一時workspaceと制御可能なtest doubleを使用する。
- 外部Providerとの境界はMockまたはFakeへ置き換え、通常のCIで再現可能にする。

### End-to-End Test

- build済みbinaryを入口として、利用者から見た主要なworkflow全体を検証する。
- 通常のCIへ追加する場合は、MockまたはFake Providerを使用し、認証情報、課金、外部serviceの可用性に依存させない。
- 実行時間や不安定性が通常のCIを阻害する場合は、必須jobから分離する。

### Live Provider Test

- 実際の外部ProviderやAgentを呼び出すテストは、通常のPull Request CIでは実行しない。
- 必要な場合は、明示的な手動実行または定期実行の別jobとし、認証情報、利用枠、費用、timeoutを管理する。
- Live Provider Testの失敗を、MockまたはFakeで再現可能な契約テストの代わりにしない。

## unsafe

- project内のcrateは`#![deny(unsafe_code)]`を初期設定とし、unsafe codeを原則禁止する。
- unsafeが必要な場合は、実装前にDesign / RFC Issueで必要性、安全条件、代替案、影響範囲を確認する。
- 承認された例外は最小scopeに限定し、`#[allow(unsafe_code)]`と`// SAFETY:` commentで成立条件を記載し、その条件を確認するtestを追加する。
- `forbid(unsafe_code)`ではなく`deny(unsafe_code)`を使用し、合意済みの例外だけを局所的に許可できるようにする。
- 依存crate内部のunsafe codeはこの禁止対象に含めない。依存関係の選定や監査は別の方針として扱う。

## CIへの反映範囲

後続の基本CI構築Issueでは、「必須の検証」に記載した3コマンドを自動実行します。Live Provider Test、追加lint群、対応platformのmatrix、coverage基準は、初期CIの必須範囲に含めません。

## 参考資料

- [cargo fmt - The Cargo Book](https://doc.rust-lang.org/cargo/commands/cargo-fmt.html)
- [Continuous Integration - Clippy Documentation](https://doc.rust-lang.org/clippy/continuous_integration/index.html)
- [Test Organization - The Rust Programming Language](https://doc.rust-lang.org/book/ch11-03-test-organization.html)
- [Diagnostics - The Rust Reference](https://doc.rust-lang.org/reference/attributes/diagnostics.html)
