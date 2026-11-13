# Voice Button: open (or close) the phone server to the internet.
#
#   -Action enable  : Windows Firewall rule for the port + best-effort UPnP
#                     port mapping on the router, then a state file.
#   -Action disable : removes both, whatever the state file says.
#   -Action status  : prints the state file.
#
# The script re-launches itself elevated when it has to change anything, so the
# caller only runs one command and waits for the exit code. Everything it did is
# written to -StatePath, and a human-readable log sits next to it.

param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('enable', 'disable', 'status')]
    [string]$Action,

    [int]$Port = 8443,

    # Where the result is written. The app owns this file otherwise.
    [string]$StatePath,

    # Address a phone in the local network would use; UPnP maps this one.
    [string]$InternalIp = '',

    # Skip the router part (used when the machine already holds a public
    # address: there is nothing to forward).
    [switch]$SkipUpnp,

    [string]$RuleName = 'Voice Button Site'
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

if (-not $StatePath) {
    $StatePath = Join-Path $PSScriptRoot 'network.json'
}
$LogPath = [System.IO.Path]::ChangeExtension($StatePath, '.log')

function Write-Log([string]$Message) {
    $line = '[' + (Get-Date -Format 'yyyy-MM-dd HH:mm:ss') + '] ' + $Message
    try { Add-Content -Path $LogPath -Value $line -Encoding UTF8 } catch { }
}

function Test-Admin {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Read-State {
    if (Test-Path $StatePath) {
        try { return Get-Content -Path $StatePath -Raw -Encoding UTF8 | ConvertFrom-Json } catch { }
    }
    return $null
}

function Write-State($state) {
    $json = $state | ConvertTo-Json -Depth 5
    $dir = Split-Path -Parent $StatePath
    if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    [System.IO.File]::WriteAllText($StatePath, $json, (New-Object System.Text.UTF8Encoding($false)))
}

function Get-LocalIp {
    if ($InternalIp) { return $InternalIp }
    $address = Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue |
        Where-Object { $_.IPAddress -notlike '127.*' -and $_.IPAddress -notlike '169.254.*' } |
        Sort-Object -Property SkipAsSource, InterfaceMetric |
        Select-Object -First 1
    if ($address) { return $address.IPAddress }
    return '127.0.0.1'
}

# --- UPnP -------------------------------------------------------------------
# Discovery by SSDP, then one SOAP call to the router's WAN connection service.

function Find-UpnpRouter {
    $socket = New-Object System.Net.Sockets.UdpClient
    try {
        $socket.Client.ReceiveTimeout = 3000
        $message = "M-SEARCH * HTTP/1.1`r`nHOST: 239.255.255.250:1900`r`nMAN: `"ssdp:discover`"`r`nMX: 2`r`nST: urn:schemas-upnp-org:device:InternetGatewayDevice:1`r`n`r`n"
        $bytes = [System.Text.Encoding]::ASCII.GetBytes($message)
        $endpoint = New-Object System.Net.IPEndPoint([System.Net.IPAddress]::Parse('239.255.255.250'), 1900)
        [void]$socket.Send($bytes, $bytes.Length, $endpoint)
        $deadline = (Get-Date).AddSeconds(3)
        while ((Get-Date) -lt $deadline) {
            try {
                $remote = New-Object System.Net.IPEndPoint([System.Net.IPAddress]::Any, 0)
                $reply = $socket.Receive([ref]$remote)
                $text = [System.Text.Encoding]::ASCII.GetString($reply)
                if ($text -match '(?im)^LOCATION:\s*(\S+)') {
                    return @{ Location = $Matches[1]; Router = $remote.Address.ToString() }
                }
            } catch {
                break
            }
        }
    } catch {
        Write-Log "UPnP discovery failed: $($_.Exception.Message)"
    } finally {
        $socket.Close()
    }
    return $null
}

function Get-WanService([string]$location) {
    try {
        $response = Invoke-WebRequest -Uri $location -UseBasicParsing -TimeoutSec 5
        [xml]$xml = $response.Content
        $base = [System.Uri]::new($location)
        $candidates = $xml.SelectNodes('//*[local-name()="service"]')
        foreach ($service in $candidates) {
            $type = $service.SelectSingleNode('*[local-name()="serviceType"]').InnerText
            if ($type -notmatch 'WAN(IP|PPP)Connection') { continue }
            $control = $service.SelectSingleNode('*[local-name()="controlURL"]').InnerText
            $url = if ($control.StartsWith('http')) { $control } else { [System.Uri]::new($base, $control).AbsoluteUri }
            return @{ Url = $url; Type = $type }
        }
    } catch {
        Write-Log "UPnP description failed: $($_.Exception.Message)"
    }
    return $null
}

function Invoke-Soap([string]$url, [string]$serviceType, [string]$action, [string]$body) {
    $envelope = @"
<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body><u:$action xmlns:u="$serviceType">$body</u:$action></s:Body>
</s:Envelope>
"@
    $headers = @{
        'Content-Type' = 'text/xml; charset="utf-8"'
        SOAPAction     = '"' + $serviceType + '#' + $action + '"'
    }
    Invoke-WebRequest -Uri $url -Method Post -Body $envelope -Headers $headers -UseBasicParsing -TimeoutSec 8 | Out-Null
}

function Add-PortMapping([int]$port, [string]$internal) {
    $router = Find-UpnpRouter
    if (-not $router) { Write-Log 'UPnP: no router answered'; return $null }
    $service = Get-WanService $router.Location
    if (-not $service) { Write-Log 'UPnP: no WAN connection service'; return $null }
    $body = "<NewRemoteHost></NewRemoteHost><NewExternalPort>$port</NewExternalPort>" +
        "<NewProtocol>TCP</NewProtocol><NewInternalPort>$port</NewInternalPort>" +
        "<NewInternalClient>$internal</NewInternalClient><NewEnabled>1</NewEnabled>" +
        "<NewPortMappingDescription>Voice Button</NewPortMappingDescription><NewLeaseDuration>0</NewLeaseDuration>"
    try {
        Invoke-Soap $service.Url $service.Type 'AddPortMapping' $body
        Write-Log "UPnP: mapped TCP $port to ${internal}:$port on $($router.Router)"
        return @{ router = $router.Router; external_port = $port; internal_ip = $internal; added = $true }
    } catch {
        Write-Log "UPnP AddPortMapping failed: $($_.Exception.Message)"
        return $null
    }
}

function Remove-PortMapping([int]$port, $state) {
    if (-not $state -or -not $state.upnp) { return }
    $router = Find-UpnpRouter
    if (-not $router) { return }
    $service = Get-WanService $router.Location
    if (-not $service) { return }
    $body = "<NewRemoteHost></NewRemoteHost><NewExternalPort>$($state.upnp.external_port)</NewExternalPort><NewProtocol>TCP</NewProtocol>"
    try {
        Invoke-Soap $service.Url $service.Type 'DeletePortMapping' $body
        Write-Log "UPnP: mapping for $($state.upnp.external_port) removed"
    } catch {
        Write-Log "UPnP DeletePortMapping failed: $($_.Exception.Message)"
    }
}

# --- actions ----------------------------------------------------------------

function Invoke-Status {
    $state = Read-State
    if ($state) { $state | ConvertTo-Json -Depth 5 } else { '{}' }
}

function Invoke-Enable {
    if (-not (Test-Admin)) {
        $arguments = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"",
            '-Action', 'enable', '-Port', $Port, '-StatePath', "`"$StatePath`"", '-RuleName', "`"$RuleName`"")
        if ($InternalIp) { $arguments += @('-InternalIp', "`"$InternalIp`"") }
        if ($SkipUpnp) { $arguments += '-SkipUpnp' }
        Write-Log 'elevation requested'
        $child = Start-Process -FilePath 'powershell.exe' -Verb RunAs -Wait -PassThru -ArgumentList $arguments
        exit $child.ExitCode
    }

    $ports = @($Port)
    $portList = ($ports -join ',')
    Write-Log "enable: port $portList"
    $state = [ordered]@{
        port            = $Port
        enabled         = $true
        firewall_rule   = $RuleName
        firewall_created = (Get-Date).ToString('o')
        upnp            = $null
        error           = $null
        updated         = (Get-Date).ToString('o')
    }

    # Remove any rule of ours from an earlier port first: nothing may leak.
    try {
        Get-NetFirewallRule -DisplayName $RuleName -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    } catch {
        Write-Log "firewall cleanup: $($_.Exception.Message)"
    }
    try {
        # -LocalPort wants an array of port numbers: a comma-joined string is
        # rejected with "The port is invalid."
        New-NetFirewallRule -DisplayName $RuleName -Direction Inbound -Action Allow -Protocol TCP `
            -LocalPort $ports -Profile Any -Description 'Voice Button phone server' | Out-Null
        Write-Log "firewall rule created for TCP $portList"
    } catch {
        Write-Log "firewall rule failed: $($_.Exception.Message)"
        $state.error = 'firewall: ' + $_.Exception.Message
        # Nothing is open, so the state must not claim otherwise.
        $state.enabled = $false
    }

    if (-not $SkipUpnp) {
        $state.upnp = Add-PortMapping -port $Port -internal (Get-LocalIp)
        if (-not $state.upnp) {
            $state.error = if ($state.error) { $state.error + '; upnp unavailable' } else { 'upnp unavailable' }
        }
    } else {
        Write-Log 'UPnP skipped: the machine holds a public address itself'
    }

    Write-State $state
    $state | ConvertTo-Json -Depth 5
}

function Invoke-Disable {
    $state = Read-State
    if (-not (Test-Admin)) {
        $arguments = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"",
            '-Action', 'disable', '-Port', $Port, '-StatePath', "`"$StatePath`"", '-RuleName', "`"$RuleName`"")
        Write-Log 'elevation requested'
        $child = Start-Process -FilePath 'powershell.exe' -Verb RunAs -Wait -PassThru -ArgumentList $arguments
        exit $child.ExitCode
    }

    Write-Log 'disable'
    if ($state) { Remove-PortMapping -port $Port -state $state }
    try {
        Get-NetFirewallRule -DisplayName $RuleName -ErrorAction SilentlyContinue | Remove-NetFirewallRule
        Write-Log 'firewall rules removed'
    } catch {
        Write-Log "firewall cleanup failed: $($_.Exception.Message)"
    }
    Write-State ([ordered]@{
            port    = $Port
            enabled = $false
            upnp    = $null
            error   = $null
            updated = (Get-Date).ToString('o')
        })
}

try {
    switch ($Action) {
        'enable' { Invoke-Enable | Out-Null }
        'disable' { Invoke-Disable }
        'status' { Invoke-Status }
    }
    exit 0
} catch {
    Write-Log "fatal: $($_.Exception.Message)"
    Write-State ([ordered]@{
            port    = $Port
            enabled = $false
            error   = $_.Exception.Message
            updated = (Get-Date).ToString('o')
        })
    exit 1
}
