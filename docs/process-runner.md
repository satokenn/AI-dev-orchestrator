# ProcessRunner の停止と出力回収

`ProcessRunner` は実行ごとに専用プロセスグループを作り、timeout と `CancellationToken` を同じ停止手順で扱います。停止時はまずグループへ TERM を送り、猶予後も残っていれば KILL を送り、グループ消滅と直接の子プロセス終了を確認します。確認できた場合だけ `TimedOut` / `Cancelled` として返します。確認できない場合は `Interrupted { stopped: false, ... }` を返し、成功や停止済みとは扱いません。

起動前にキャンセル済みなら子プロセスをspawnせず `CancelledBeforeStart` を返します。stdout と stderr は別々のreaderで逐次回収し、各streamの先頭1 MiBだけを保持します。上限を超えてもpipeは読み捨ててdrainし、`ProcessOutput::output_truncated` で切り詰めを示します。timeout / cancel / 停止確認不能の結果は取得済みの部分ログを保持し、Provider境界でも `ProviderError::Interrupted` のstdout / stderrへ引き継ぎます。

Linux / macOS では、起動したプロセスグループに属する子孫を対象にします。プロセスが自分で別session / process groupへ移動した場合やOSがsignalを拒否した場合、その子孫の停止を保証できず `Interrupted` になります。Windowsでは `taskkill /T /F` の成功と直接の子プロセス終了を停止確認に使います。OS全体の任意プロセスや、実行プロセスが自ら管理グループを離脱した後のプロセスは対象外です。

pipeを保持する子孫により親コマンド終了後もpipeが閉じない場合、drain猶予を超えた時点でグループ停止を試みます。回収済み部分ログと停止確認結果を返し、無期限にreader threadのjoinを待ちません。
