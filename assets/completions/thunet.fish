# Print an optspec for argparse to handle cmd's options that are independent of any subcommand.
function __fish_thunet_global_optspecs
    string join \n host= http timeout= u/username= p/password= ip= ac-id= minimized state-dir= admin-error= h/help V/version
end

function __fish_thunet_needs_command
    # Figure out if the current invocation already has a command.
    set -l cmd (commandline -opc)
    set -e cmd[1]
    argparse -s (__fish_thunet_global_optspecs) -- $cmd 2>/dev/null
    or return
    if set -q argv[1]
        # Also print the command, so this can be used to figure out what it is.
        echo $argv[1]
        return 1
    end
    return 0
end

function __fish_thunet_using_subcommand
    set -l cmd (__fish_thunet_needs_command)
    test -z "$cmd"
    and return 1
    contains -- $cmd[1] $argv
end

complete -c thunet -n "__fish_thunet_needs_command" -l host -d 'Portal host (overrides the config file)' -r
complete -c thunet -n "__fish_thunet_needs_command" -l timeout -d 'Request timeout in seconds' -r
complete -c thunet -n "__fish_thunet_needs_command" -s u -l username -d 'TUNET account (overrides the saved one)' -r
complete -c thunet -n "__fish_thunet_needs_command" -s p -l password -d 'Password (otherwise read from the saved credential, or prompted)' -r
complete -c thunet -n "__fish_thunet_needs_command" -l ip -d 'Local IP to authenticate; empty lets the portal infer it' -r
complete -c thunet -n "__fish_thunet_needs_command" -l ac-id -d 'ac_id override; empty means auto-detect' -r
complete -c thunet -n "__fish_thunet_needs_command" -l state-dir -d 'Where `config.json` and `credential.bin` live. The Windows service is registered with the installing user\'s directory, because the token it runs under has no profile of its own' -r -F
complete -c thunet -n "__fish_thunet_needs_command" -l admin-error -d 'Internal: file where the elevated helper records a failure' -r -F
complete -c thunet -n "__fish_thunet_needs_command" -l http -d 'Use plain http instead of https'
complete -c thunet -n "__fish_thunet_needs_command" -l minimized -d 'Start minimised to the tray (used by the autostart entry)'
complete -c thunet -n "__fish_thunet_needs_command" -s h -l help -d 'Print help'
complete -c thunet -n "__fish_thunet_needs_command" -s V -l version -d 'Print version'
complete -c thunet -n "__fish_thunet_needs_command" -f -a "selftest" -d 'Offline protocol self-test (see `cargo test`)'
complete -c thunet -n "__fish_thunet_needs_command" -f -a "ac-id" -d 'Print the portal ac_id the running service is using'
complete -c thunet -n "__fish_thunet_needs_command" -f -a "status" -d 'Show portal session / traffic information'
complete -c thunet -n "__fish_thunet_needs_command" -f -a "login" -d 'Ask the running service to authenticate now'
complete -c thunet -n "__fish_thunet_needs_command" -f -a "logout" -d 'Ask the running service to de-authenticate'
complete -c thunet -n "__fish_thunet_needs_command" -f -a "core" -d 'Run the background service (no window, no tray)'
complete -c thunet -n "__fish_thunet_needs_command" -f -a "service" -d 'Install or control the background service'
complete -c thunet -n "__fish_thunet_needs_command" -f -a "service-admin" -d 'Perform one privileged service operation (the elevated half of `service <action>`, not for humans)'
complete -c thunet -n "__fish_thunet_needs_command" -f -a "service-resume" -d 'Restart the core after an upgrade if it starts at boot (installer internal)'
complete -c thunet -n "__fish_thunet_needs_command" -f -a "config" -d 'Print the config path and effective settings'
complete -c thunet -n "__fish_thunet_needs_command" -f -a "shortcut" -d 'Create a desktop shortcut for this executable'
complete -c thunet -n "__fish_thunet_needs_command" -f -a "completions" -d 'Print a shell completion script'
complete -c thunet -n "__fish_thunet_needs_command" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c thunet -n "__fish_thunet_using_subcommand selftest" -l host -d 'Portal host (overrides the config file)' -r
complete -c thunet -n "__fish_thunet_using_subcommand selftest" -l timeout -d 'Request timeout in seconds' -r
complete -c thunet -n "__fish_thunet_using_subcommand selftest" -s u -l username -d 'TUNET account (overrides the saved one)' -r
complete -c thunet -n "__fish_thunet_using_subcommand selftest" -s p -l password -d 'Password (otherwise read from the saved credential, or prompted)' -r
complete -c thunet -n "__fish_thunet_using_subcommand selftest" -l ip -d 'Local IP to authenticate; empty lets the portal infer it' -r
complete -c thunet -n "__fish_thunet_using_subcommand selftest" -l ac-id -d 'ac_id override; empty means auto-detect' -r
complete -c thunet -n "__fish_thunet_using_subcommand selftest" -l state-dir -d 'Where `config.json` and `credential.bin` live. The Windows service is registered with the installing user\'s directory, because the token it runs under has no profile of its own' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand selftest" -l admin-error -d 'Internal: file where the elevated helper records a failure' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand selftest" -l http -d 'Use plain http instead of https'
complete -c thunet -n "__fish_thunet_using_subcommand selftest" -l minimized -d 'Start minimised to the tray (used by the autostart entry)'
complete -c thunet -n "__fish_thunet_using_subcommand selftest" -s h -l help -d 'Print help'
complete -c thunet -n "__fish_thunet_using_subcommand ac-id" -l host -d 'Portal host (overrides the config file)' -r
complete -c thunet -n "__fish_thunet_using_subcommand ac-id" -l timeout -d 'Request timeout in seconds' -r
complete -c thunet -n "__fish_thunet_using_subcommand ac-id" -s u -l username -d 'TUNET account (overrides the saved one)' -r
complete -c thunet -n "__fish_thunet_using_subcommand ac-id" -s p -l password -d 'Password (otherwise read from the saved credential, or prompted)' -r
complete -c thunet -n "__fish_thunet_using_subcommand ac-id" -l ip -d 'Local IP to authenticate; empty lets the portal infer it' -r
complete -c thunet -n "__fish_thunet_using_subcommand ac-id" -l ac-id -d 'ac_id override; empty means auto-detect' -r
complete -c thunet -n "__fish_thunet_using_subcommand ac-id" -l state-dir -d 'Where `config.json` and `credential.bin` live. The Windows service is registered with the installing user\'s directory, because the token it runs under has no profile of its own' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand ac-id" -l admin-error -d 'Internal: file where the elevated helper records a failure' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand ac-id" -l http -d 'Use plain http instead of https'
complete -c thunet -n "__fish_thunet_using_subcommand ac-id" -l minimized -d 'Start minimised to the tray (used by the autostart entry)'
complete -c thunet -n "__fish_thunet_using_subcommand ac-id" -s h -l help -d 'Print help'
complete -c thunet -n "__fish_thunet_using_subcommand status" -l host -d 'Portal host (overrides the config file)' -r
complete -c thunet -n "__fish_thunet_using_subcommand status" -l timeout -d 'Request timeout in seconds' -r
complete -c thunet -n "__fish_thunet_using_subcommand status" -s u -l username -d 'TUNET account (overrides the saved one)' -r
complete -c thunet -n "__fish_thunet_using_subcommand status" -s p -l password -d 'Password (otherwise read from the saved credential, or prompted)' -r
complete -c thunet -n "__fish_thunet_using_subcommand status" -l ip -d 'Local IP to authenticate; empty lets the portal infer it' -r
complete -c thunet -n "__fish_thunet_using_subcommand status" -l ac-id -d 'ac_id override; empty means auto-detect' -r
complete -c thunet -n "__fish_thunet_using_subcommand status" -l state-dir -d 'Where `config.json` and `credential.bin` live. The Windows service is registered with the installing user\'s directory, because the token it runs under has no profile of its own' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand status" -l admin-error -d 'Internal: file where the elevated helper records a failure' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand status" -l http -d 'Use plain http instead of https'
complete -c thunet -n "__fish_thunet_using_subcommand status" -l minimized -d 'Start minimised to the tray (used by the autostart entry)'
complete -c thunet -n "__fish_thunet_using_subcommand status" -s h -l help -d 'Print help'
complete -c thunet -n "__fish_thunet_using_subcommand login" -l host -d 'Portal host (overrides the config file)' -r
complete -c thunet -n "__fish_thunet_using_subcommand login" -l timeout -d 'Request timeout in seconds' -r
complete -c thunet -n "__fish_thunet_using_subcommand login" -s u -l username -d 'TUNET account (overrides the saved one)' -r
complete -c thunet -n "__fish_thunet_using_subcommand login" -s p -l password -d 'Password (otherwise read from the saved credential, or prompted)' -r
complete -c thunet -n "__fish_thunet_using_subcommand login" -l ip -d 'Local IP to authenticate; empty lets the portal infer it' -r
complete -c thunet -n "__fish_thunet_using_subcommand login" -l ac-id -d 'ac_id override; empty means auto-detect' -r
complete -c thunet -n "__fish_thunet_using_subcommand login" -l state-dir -d 'Where `config.json` and `credential.bin` live. The Windows service is registered with the installing user\'s directory, because the token it runs under has no profile of its own' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand login" -l admin-error -d 'Internal: file where the elevated helper records a failure' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand login" -l http -d 'Use plain http instead of https'
complete -c thunet -n "__fish_thunet_using_subcommand login" -l minimized -d 'Start minimised to the tray (used by the autostart entry)'
complete -c thunet -n "__fish_thunet_using_subcommand login" -s h -l help -d 'Print help'
complete -c thunet -n "__fish_thunet_using_subcommand logout" -l host -d 'Portal host (overrides the config file)' -r
complete -c thunet -n "__fish_thunet_using_subcommand logout" -l timeout -d 'Request timeout in seconds' -r
complete -c thunet -n "__fish_thunet_using_subcommand logout" -s u -l username -d 'TUNET account (overrides the saved one)' -r
complete -c thunet -n "__fish_thunet_using_subcommand logout" -s p -l password -d 'Password (otherwise read from the saved credential, or prompted)' -r
complete -c thunet -n "__fish_thunet_using_subcommand logout" -l ip -d 'Local IP to authenticate; empty lets the portal infer it' -r
complete -c thunet -n "__fish_thunet_using_subcommand logout" -l ac-id -d 'ac_id override; empty means auto-detect' -r
complete -c thunet -n "__fish_thunet_using_subcommand logout" -l state-dir -d 'Where `config.json` and `credential.bin` live. The Windows service is registered with the installing user\'s directory, because the token it runs under has no profile of its own' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand logout" -l admin-error -d 'Internal: file where the elevated helper records a failure' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand logout" -l http -d 'Use plain http instead of https'
complete -c thunet -n "__fish_thunet_using_subcommand logout" -l minimized -d 'Start minimised to the tray (used by the autostart entry)'
complete -c thunet -n "__fish_thunet_using_subcommand logout" -s h -l help -d 'Print help'
complete -c thunet -n "__fish_thunet_using_subcommand core" -l host -d 'Portal host (overrides the config file)' -r
complete -c thunet -n "__fish_thunet_using_subcommand core" -l timeout -d 'Request timeout in seconds' -r
complete -c thunet -n "__fish_thunet_using_subcommand core" -s u -l username -d 'TUNET account (overrides the saved one)' -r
complete -c thunet -n "__fish_thunet_using_subcommand core" -s p -l password -d 'Password (otherwise read from the saved credential, or prompted)' -r
complete -c thunet -n "__fish_thunet_using_subcommand core" -l ip -d 'Local IP to authenticate; empty lets the portal infer it' -r
complete -c thunet -n "__fish_thunet_using_subcommand core" -l ac-id -d 'ac_id override; empty means auto-detect' -r
complete -c thunet -n "__fish_thunet_using_subcommand core" -l state-dir -d 'Where `config.json` and `credential.bin` live. The Windows service is registered with the installing user\'s directory, because the token it runs under has no profile of its own' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand core" -l admin-error -d 'Internal: file where the elevated helper records a failure' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand core" -l service -d 'Started by the Windows service manager; not for humans'
complete -c thunet -n "__fish_thunet_using_subcommand core" -l machine -d 'Run against the machine-wide state directory and endpoint. The Linux system unit passes this; a user-run core stays portable'
complete -c thunet -n "__fish_thunet_using_subcommand core" -l http -d 'Use plain http instead of https'
complete -c thunet -n "__fish_thunet_using_subcommand core" -l minimized -d 'Start minimised to the tray (used by the autostart entry)'
complete -c thunet -n "__fish_thunet_using_subcommand core" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c thunet -n "__fish_thunet_using_subcommand service" -l host -d 'Portal host (overrides the config file)' -r
complete -c thunet -n "__fish_thunet_using_subcommand service" -l timeout -d 'Request timeout in seconds' -r
complete -c thunet -n "__fish_thunet_using_subcommand service" -s u -l username -d 'TUNET account (overrides the saved one)' -r
complete -c thunet -n "__fish_thunet_using_subcommand service" -s p -l password -d 'Password (otherwise read from the saved credential, or prompted)' -r
complete -c thunet -n "__fish_thunet_using_subcommand service" -l ip -d 'Local IP to authenticate; empty lets the portal infer it' -r
complete -c thunet -n "__fish_thunet_using_subcommand service" -l ac-id -d 'ac_id override; empty means auto-detect' -r
complete -c thunet -n "__fish_thunet_using_subcommand service" -l state-dir -d 'Where `config.json` and `credential.bin` live. The Windows service is registered with the installing user\'s directory, because the token it runs under has no profile of its own' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand service" -l admin-error -d 'Internal: file where the elevated helper records a failure' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand service" -l http -d 'Use plain http instead of https'
complete -c thunet -n "__fish_thunet_using_subcommand service" -l minimized -d 'Start minimised to the tray (used by the autostart entry)'
complete -c thunet -n "__fish_thunet_using_subcommand service" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c thunet -n "__fish_thunet_using_subcommand service-admin" -l host -d 'Portal host (overrides the config file)' -r
complete -c thunet -n "__fish_thunet_using_subcommand service-admin" -l timeout -d 'Request timeout in seconds' -r
complete -c thunet -n "__fish_thunet_using_subcommand service-admin" -s u -l username -d 'TUNET account (overrides the saved one)' -r
complete -c thunet -n "__fish_thunet_using_subcommand service-admin" -s p -l password -d 'Password (otherwise read from the saved credential, or prompted)' -r
complete -c thunet -n "__fish_thunet_using_subcommand service-admin" -l ip -d 'Local IP to authenticate; empty lets the portal infer it' -r
complete -c thunet -n "__fish_thunet_using_subcommand service-admin" -l ac-id -d 'ac_id override; empty means auto-detect' -r
complete -c thunet -n "__fish_thunet_using_subcommand service-admin" -l state-dir -d 'Where `config.json` and `credential.bin` live. The Windows service is registered with the installing user\'s directory, because the token it runs under has no profile of its own' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand service-admin" -l admin-error -d 'Internal: file where the elevated helper records a failure' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand service-admin" -l http -d 'Use plain http instead of https'
complete -c thunet -n "__fish_thunet_using_subcommand service-admin" -l minimized -d 'Start minimised to the tray (used by the autostart entry)'
complete -c thunet -n "__fish_thunet_using_subcommand service-admin" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c thunet -n "__fish_thunet_using_subcommand service-resume" -l host -d 'Portal host (overrides the config file)' -r
complete -c thunet -n "__fish_thunet_using_subcommand service-resume" -l timeout -d 'Request timeout in seconds' -r
complete -c thunet -n "__fish_thunet_using_subcommand service-resume" -s u -l username -d 'TUNET account (overrides the saved one)' -r
complete -c thunet -n "__fish_thunet_using_subcommand service-resume" -s p -l password -d 'Password (otherwise read from the saved credential, or prompted)' -r
complete -c thunet -n "__fish_thunet_using_subcommand service-resume" -l ip -d 'Local IP to authenticate; empty lets the portal infer it' -r
complete -c thunet -n "__fish_thunet_using_subcommand service-resume" -l ac-id -d 'ac_id override; empty means auto-detect' -r
complete -c thunet -n "__fish_thunet_using_subcommand service-resume" -l state-dir -d 'Where `config.json` and `credential.bin` live. The Windows service is registered with the installing user\'s directory, because the token it runs under has no profile of its own' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand service-resume" -l admin-error -d 'Internal: file where the elevated helper records a failure' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand service-resume" -l http -d 'Use plain http instead of https'
complete -c thunet -n "__fish_thunet_using_subcommand service-resume" -l minimized -d 'Start minimised to the tray (used by the autostart entry)'
complete -c thunet -n "__fish_thunet_using_subcommand service-resume" -s h -l help -d 'Print help'
complete -c thunet -n "__fish_thunet_using_subcommand config" -l host -d 'Portal host (overrides the config file)' -r
complete -c thunet -n "__fish_thunet_using_subcommand config" -l timeout -d 'Request timeout in seconds' -r
complete -c thunet -n "__fish_thunet_using_subcommand config" -s u -l username -d 'TUNET account (overrides the saved one)' -r
complete -c thunet -n "__fish_thunet_using_subcommand config" -s p -l password -d 'Password (otherwise read from the saved credential, or prompted)' -r
complete -c thunet -n "__fish_thunet_using_subcommand config" -l ip -d 'Local IP to authenticate; empty lets the portal infer it' -r
complete -c thunet -n "__fish_thunet_using_subcommand config" -l ac-id -d 'ac_id override; empty means auto-detect' -r
complete -c thunet -n "__fish_thunet_using_subcommand config" -l state-dir -d 'Where `config.json` and `credential.bin` live. The Windows service is registered with the installing user\'s directory, because the token it runs under has no profile of its own' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand config" -l admin-error -d 'Internal: file where the elevated helper records a failure' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand config" -l http -d 'Use plain http instead of https'
complete -c thunet -n "__fish_thunet_using_subcommand config" -l minimized -d 'Start minimised to the tray (used by the autostart entry)'
complete -c thunet -n "__fish_thunet_using_subcommand config" -s h -l help -d 'Print help'
complete -c thunet -n "__fish_thunet_using_subcommand shortcut" -l host -d 'Portal host (overrides the config file)' -r
complete -c thunet -n "__fish_thunet_using_subcommand shortcut" -l timeout -d 'Request timeout in seconds' -r
complete -c thunet -n "__fish_thunet_using_subcommand shortcut" -s u -l username -d 'TUNET account (overrides the saved one)' -r
complete -c thunet -n "__fish_thunet_using_subcommand shortcut" -s p -l password -d 'Password (otherwise read from the saved credential, or prompted)' -r
complete -c thunet -n "__fish_thunet_using_subcommand shortcut" -l ip -d 'Local IP to authenticate; empty lets the portal infer it' -r
complete -c thunet -n "__fish_thunet_using_subcommand shortcut" -l ac-id -d 'ac_id override; empty means auto-detect' -r
complete -c thunet -n "__fish_thunet_using_subcommand shortcut" -l state-dir -d 'Where `config.json` and `credential.bin` live. The Windows service is registered with the installing user\'s directory, because the token it runs under has no profile of its own' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand shortcut" -l admin-error -d 'Internal: file where the elevated helper records a failure' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand shortcut" -l http -d 'Use plain http instead of https'
complete -c thunet -n "__fish_thunet_using_subcommand shortcut" -l minimized -d 'Start minimised to the tray (used by the autostart entry)'
complete -c thunet -n "__fish_thunet_using_subcommand shortcut" -s h -l help -d 'Print help'
complete -c thunet -n "__fish_thunet_using_subcommand completions" -l host -d 'Portal host (overrides the config file)' -r
complete -c thunet -n "__fish_thunet_using_subcommand completions" -l timeout -d 'Request timeout in seconds' -r
complete -c thunet -n "__fish_thunet_using_subcommand completions" -s u -l username -d 'TUNET account (overrides the saved one)' -r
complete -c thunet -n "__fish_thunet_using_subcommand completions" -s p -l password -d 'Password (otherwise read from the saved credential, or prompted)' -r
complete -c thunet -n "__fish_thunet_using_subcommand completions" -l ip -d 'Local IP to authenticate; empty lets the portal infer it' -r
complete -c thunet -n "__fish_thunet_using_subcommand completions" -l ac-id -d 'ac_id override; empty means auto-detect' -r
complete -c thunet -n "__fish_thunet_using_subcommand completions" -l state-dir -d 'Where `config.json` and `credential.bin` live. The Windows service is registered with the installing user\'s directory, because the token it runs under has no profile of its own' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand completions" -l admin-error -d 'Internal: file where the elevated helper records a failure' -r -F
complete -c thunet -n "__fish_thunet_using_subcommand completions" -l http -d 'Use plain http instead of https'
complete -c thunet -n "__fish_thunet_using_subcommand completions" -l minimized -d 'Start minimised to the tray (used by the autostart entry)'
complete -c thunet -n "__fish_thunet_using_subcommand completions" -s h -l help -d 'Print help (see more with \'--help\')'
complete -c thunet -n "__fish_thunet_using_subcommand help; and not __fish_seen_subcommand_from selftest ac-id status login logout core service service-admin service-resume config shortcut completions help" -f -a "selftest" -d 'Offline protocol self-test (see `cargo test`)'
complete -c thunet -n "__fish_thunet_using_subcommand help; and not __fish_seen_subcommand_from selftest ac-id status login logout core service service-admin service-resume config shortcut completions help" -f -a "ac-id" -d 'Print the portal ac_id the running service is using'
complete -c thunet -n "__fish_thunet_using_subcommand help; and not __fish_seen_subcommand_from selftest ac-id status login logout core service service-admin service-resume config shortcut completions help" -f -a "status" -d 'Show portal session / traffic information'
complete -c thunet -n "__fish_thunet_using_subcommand help; and not __fish_seen_subcommand_from selftest ac-id status login logout core service service-admin service-resume config shortcut completions help" -f -a "login" -d 'Ask the running service to authenticate now'
complete -c thunet -n "__fish_thunet_using_subcommand help; and not __fish_seen_subcommand_from selftest ac-id status login logout core service service-admin service-resume config shortcut completions help" -f -a "logout" -d 'Ask the running service to de-authenticate'
complete -c thunet -n "__fish_thunet_using_subcommand help; and not __fish_seen_subcommand_from selftest ac-id status login logout core service service-admin service-resume config shortcut completions help" -f -a "core" -d 'Run the background service (no window, no tray)'
complete -c thunet -n "__fish_thunet_using_subcommand help; and not __fish_seen_subcommand_from selftest ac-id status login logout core service service-admin service-resume config shortcut completions help" -f -a "service" -d 'Install or control the background service'
complete -c thunet -n "__fish_thunet_using_subcommand help; and not __fish_seen_subcommand_from selftest ac-id status login logout core service service-admin service-resume config shortcut completions help" -f -a "service-admin" -d 'Perform one privileged service operation (the elevated half of `service <action>`, not for humans)'
complete -c thunet -n "__fish_thunet_using_subcommand help; and not __fish_seen_subcommand_from selftest ac-id status login logout core service service-admin service-resume config shortcut completions help" -f -a "service-resume" -d 'Restart the core after an upgrade if it starts at boot (installer internal)'
complete -c thunet -n "__fish_thunet_using_subcommand help; and not __fish_seen_subcommand_from selftest ac-id status login logout core service service-admin service-resume config shortcut completions help" -f -a "config" -d 'Print the config path and effective settings'
complete -c thunet -n "__fish_thunet_using_subcommand help; and not __fish_seen_subcommand_from selftest ac-id status login logout core service service-admin service-resume config shortcut completions help" -f -a "shortcut" -d 'Create a desktop shortcut for this executable'
complete -c thunet -n "__fish_thunet_using_subcommand help; and not __fish_seen_subcommand_from selftest ac-id status login logout core service service-admin service-resume config shortcut completions help" -f -a "completions" -d 'Print a shell completion script'
complete -c thunet -n "__fish_thunet_using_subcommand help; and not __fish_seen_subcommand_from selftest ac-id status login logout core service service-admin service-resume config shortcut completions help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
