$cofre = "C:\Program Files\Cofre\cofre.exe"
$args = 'backup "C:\Empresa" --exclude-file "C:\ProgramData\Cofre\excludes.txt"'
$action = New-ScheduledTaskAction -Execute $cofre -Argument $args
$trigger = New-ScheduledTaskTrigger -Daily -At 2:30am
$settings = New-ScheduledTaskSettingsSet -StartWhenAvailable -RunOnlyIfNetworkAvailable
[Environment]::SetEnvironmentVariable("COFRE_REPO", "https://cofre.seudominio.com.br", "Machine")
[Environment]::SetEnvironmentVariable("COFRE_PASSWORD_FILE", "C:\ProgramData\Cofre\senha", "Machine")
[Environment]::SetEnvironmentVariable("COFRE_SERVER_TOKEN_FILE", "C:\ProgramData\Cofre\token", "Machine")
Register-ScheduledTask -TaskName "Cofre Backup" -Action $action -Trigger $trigger -Settings $settings -User "SYSTEM" -RunLevel Highest
