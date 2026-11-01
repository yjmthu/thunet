# Desktop notification for a finished background job.
#
# Windows has no `notify-send`. Three ways to say "it is done", in order of how
# little they interrupt:
#
#   toast : a Windows toast, through the WinRT API PowerShell already ships
#           with. Silent in the corner, stays in the Action Center, and needs no
#           module - but an *elevated* process cannot show one (toasts belong to
#           the interactive session), and they are dropped when notifications
#           are off.
#   box   : a topmost message box. Impossible to miss, works from anywhere, and
#           it steals the focus - which is exactly why it is not the default.
#
#   pwsh -NoProfile -File scripts/notify.ps1 -Title "THUnet" -Message "msi built"
#
# `auto` tries the toast and falls back to the box, so a build never finishes in
# silence. Exit code is 0 when something was shown, 1 when nothing could be.

param(
    [Parameter(Mandatory)][string]$Title,
    [string]$Message = "",
    [ValidateSet('auto', 'toast', 'box')][string]$How = 'auto'
)

$shown = $false

function Show-Toast([string]$title, [string]$text) {
    [void][Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime]
    [void][Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime]
    $template = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent(
        [Windows.UI.Notifications.ToastTemplateType]::ToastText02)
    $nodes = $template.GetElementsByTagName('text')
    [void]$nodes.Item(0).AppendChild($template.CreateTextNode($title))
    [void]$nodes.Item(1).AppendChild($template.CreateTextNode($text))
    $toast = [Windows.UI.Notifications.ToastNotification]::new($template)
    # PowerShell's own AppUserModelID: it is registered with the shell on every
    # Windows 10/11 install, so a toast needs no shortcut of our own.
    $app_id = '{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe'
    [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($app_id).Show($toast)
}

function Show-Box([string]$title, [string]$text) {
    Add-Type -AssemblyName System.Windows.Forms
    [System.Media.SystemSounds]::Asterisk.Play()
    [void][System.Windows.Forms.MessageBox]::Show(
        $text, $title,
        [System.Windows.Forms.MessageBoxButtons]::OK,
        [System.Windows.Forms.MessageBoxIcon]::Information,
        [System.Windows.Forms.MessageBoxDefaultButton]::Button1,
        [System.Windows.Forms.MessageBoxOptions]::ServiceNotification)
}

if ($How -ne 'box') {
    try {
        Show-Toast $Title $Message
        $shown = $true
    } catch {
        if ($How -eq 'toast') {
            Write-Error "toast failed: $($_.Exception.Message)"
            exit 1
        }
    }
}
if (-not $shown) {
    try {
        Show-Box $Title $Message
        $shown = $true
    } catch {
        Write-Error "message box failed: $($_.Exception.Message)"
    }
}

if ($shown) { Write-Host "notified: $Title" } else { exit 1 }
