[CmdletBinding()]
param([Parameter(Mandatory)][string]$Binary)
$ErrorActionPreference = 'Stop'
$name = 'CS2WorldCore'
if (Get-Service -Name $name -ErrorAction SilentlyContinue) { throw 'CS2WorldCore already exists on this machine' }
$exe = (Resolve-Path -LiteralPath $Binary).Path
$work = [IO.Directory]::CreateTempSubdirectory('cs2world-scm-').FullName
$config = Join-Path $work 'server.json'
$udp = [Net.Sockets.UdpClient]::new([Net.IPEndPoint]::new([Net.IPAddress]::Loopback, 0))
$port = $udp.Client.LocalEndPoint.Port
$udp.Dispose()
$server = @{inbounds=@(@{type='cs2world';listen='127.0.0.1';listen_port=$port;psk=[Guid]::NewGuid().ToString('N');a2s=@{server_name='SCM-before'}});outbounds=@(@{type='direct'})}
$server | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $config -Encoding utf8NoBOM
function Assert-ServerName([int]$EndpointPort, [string]$Expected) {
    $client = [Net.Sockets.UdpClient]::new()
    try {
        $client.Client.ReceiveTimeout = 300
        $client.Connect('127.0.0.1', $EndpointPort)
        [byte[]]$query = @(255,255,255,255,84) + [Text.Encoding]::ASCII.GetBytes('Source Engine Query') + @(0)
        $wait = [Diagnostics.Stopwatch]::StartNew()
        while ($wait.Elapsed.TotalSeconds -lt 8) {
            [void]$client.Send($query, $query.Length)
            $from = [Net.IPEndPoint]::new([Net.IPAddress]::Any, 0)
            try { $reply = $client.Receive([ref]$from) }
            catch [Net.Sockets.SocketException] { Start-Sleep -Milliseconds 50; continue }
            if ($reply.Length -lt 7 -or $reply[4] -ne 73) {continue}
            $end = [Array]::IndexOf($reply, [byte]0, 6)
            if ($end -gt 6 -and [Text.Encoding]::UTF8.GetString($reply, 6, $end - 6) -ceq $Expected) {return}
            Start-Sleep -Milliseconds 50
        }
        throw "A2S did not advertise the expected name '$Expected'"
    } finally { $client.Dispose() }
}
$installed = $false
try {
    New-Service -Name $name -BinaryPathName ('"{0}" service -c "{1}"' -f $exe,$config) -StartupType Manual | Out-Null
    $installed = $true
    Start-Service -Name $name
    (Get-Service -Name $name).WaitForStatus('Running',[TimeSpan]::FromSeconds(15))
    Assert-ServerName $port 'SCM-before'
    $server.inbounds[0].a2s.server_name = 'SCM-after'
    $server | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $config -Encoding utf8NoBOM
    sc.exe control $name paramchange | Out-Null
    if ($LASTEXITCODE -ne 0) {throw 'SCM rejected the reload control'}
    Assert-ServerName $port 'SCM-after'
    Stop-Service -Name $name
    (Get-Service -Name $name).WaitForStatus('Stopped',[TimeSpan]::FromSeconds(15))
} finally {
    if ($installed) {
        if ((Get-Service -Name $name).Status -ne 'Stopped') {Stop-Service -Name $name}
        sc.exe delete $name | Out-Null
    }
    Remove-Item -LiteralPath $config
    [IO.Directory]::Delete($work)
}
