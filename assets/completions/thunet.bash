_thunet() {
    local i cur prev opts cmd
    COMPREPLY=()
    if [[ "${BASH_VERSINFO[0]}" -ge 4 ]]; then
        cur="$2"
    else
        cur="${COMP_WORDS[COMP_CWORD]}"
    fi
    prev="$3"
    cmd=""
    opts=""

    for i in "${COMP_WORDS[@]:0:COMP_CWORD}"
    do
        case "${cmd},${i}" in
            ",$1")
                cmd="thunet"
                ;;
            thunet,ac-id)
                cmd="thunet__subcmd__ac__subcmd__id"
                ;;
            thunet,completions)
                cmd="thunet__subcmd__completions"
                ;;
            thunet,config)
                cmd="thunet__subcmd__config"
                ;;
            thunet,core)
                cmd="thunet__subcmd__core"
                ;;
            thunet,help)
                cmd="thunet__subcmd__help"
                ;;
            thunet,login)
                cmd="thunet__subcmd__login"
                ;;
            thunet,logout)
                cmd="thunet__subcmd__logout"
                ;;
            thunet,quit)
                cmd="thunet__subcmd__quit"
                ;;
            thunet,selftest)
                cmd="thunet__subcmd__selftest"
                ;;
            thunet,service)
                cmd="thunet__subcmd__service"
                ;;
            thunet,service-admin)
                cmd="thunet__subcmd__service__subcmd__admin"
                ;;
            thunet,service-purge)
                cmd="thunet__subcmd__service__subcmd__purge"
                ;;
            thunet,service-restore)
                cmd="thunet__subcmd__service__subcmd__restore"
                ;;
            thunet,shortcut)
                cmd="thunet__subcmd__shortcut"
                ;;
            thunet,status)
                cmd="thunet__subcmd__status"
                ;;
            thunet__subcmd__help,ac-id)
                cmd="thunet__subcmd__help__subcmd__ac__subcmd__id"
                ;;
            thunet__subcmd__help,completions)
                cmd="thunet__subcmd__help__subcmd__completions"
                ;;
            thunet__subcmd__help,config)
                cmd="thunet__subcmd__help__subcmd__config"
                ;;
            thunet__subcmd__help,core)
                cmd="thunet__subcmd__help__subcmd__core"
                ;;
            thunet__subcmd__help,help)
                cmd="thunet__subcmd__help__subcmd__help"
                ;;
            thunet__subcmd__help,login)
                cmd="thunet__subcmd__help__subcmd__login"
                ;;
            thunet__subcmd__help,logout)
                cmd="thunet__subcmd__help__subcmd__logout"
                ;;
            thunet__subcmd__help,quit)
                cmd="thunet__subcmd__help__subcmd__quit"
                ;;
            thunet__subcmd__help,selftest)
                cmd="thunet__subcmd__help__subcmd__selftest"
                ;;
            thunet__subcmd__help,service)
                cmd="thunet__subcmd__help__subcmd__service"
                ;;
            thunet__subcmd__help,service-admin)
                cmd="thunet__subcmd__help__subcmd__service__subcmd__admin"
                ;;
            thunet__subcmd__help,service-purge)
                cmd="thunet__subcmd__help__subcmd__service__subcmd__purge"
                ;;
            thunet__subcmd__help,service-restore)
                cmd="thunet__subcmd__help__subcmd__service__subcmd__restore"
                ;;
            thunet__subcmd__help,shortcut)
                cmd="thunet__subcmd__help__subcmd__shortcut"
                ;;
            thunet__subcmd__help,status)
                cmd="thunet__subcmd__help__subcmd__status"
                ;;
            *)
                ;;
        esac
    done

    case "${cmd}" in
        thunet)
            opts="-h -V --host --http --timeout --ip --ac-id --minimized --visible --state-dir --machine --admin-error --signal-only --help --version selftest ac-id status login logout core service service-admin service-purge service-restore quit config shortcut completions help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 1 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --host)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --timeout)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ip)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ac-id)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --state-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --admin-error)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__ac__subcmd__id)
            opts="-h --host --http --timeout --ip --ac-id --minimized --visible --state-dir --machine --admin-error --signal-only --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --host)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --timeout)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ip)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ac-id)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --state-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --admin-error)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__completions)
            opts="-h --host --http --timeout --ip --ac-id --minimized --visible --state-dir --machine --admin-error --signal-only --help bash elvish fish powershell zsh"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --host)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --timeout)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ip)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ac-id)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --state-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --admin-error)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__config)
            opts="-h --auto-connect --keep-online --remember-pwd --remember-password --host --http --timeout --ip --ac-id --minimized --visible --state-dir --machine --admin-error --signal-only --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --auto-connect)
                    COMPREPLY=($(compgen -W "on off" -- "${cur}"))
                    return 0
                    ;;
                --keep-online)
                    COMPREPLY=($(compgen -W "on off" -- "${cur}"))
                    return 0
                    ;;
                --remember-password)
                    COMPREPLY=($(compgen -W "on off" -- "${cur}"))
                    return 0
                    ;;
                --remember-pwd)
                    COMPREPLY=($(compgen -W "on off" -- "${cur}"))
                    return 0
                    ;;
                --host)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --timeout)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ip)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ac-id)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --state-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --admin-error)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__core)
            opts="-h --service --host --http --timeout --ip --ac-id --minimized --visible --state-dir --machine --admin-error --signal-only --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --host)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --timeout)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ip)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ac-id)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --state-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --admin-error)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__help)
            opts="selftest ac-id status login logout core service service-admin service-purge service-restore quit config shortcut completions help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__help__subcmd__ac__subcmd__id)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__help__subcmd__completions)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__help__subcmd__config)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__help__subcmd__core)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__help__subcmd__help)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__help__subcmd__login)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__help__subcmd__logout)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__help__subcmd__quit)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__help__subcmd__selftest)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__help__subcmd__service)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__help__subcmd__service__subcmd__admin)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__help__subcmd__service__subcmd__purge)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__help__subcmd__service__subcmd__restore)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__help__subcmd__shortcut)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__help__subcmd__status)
            opts=""
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 3 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__login)
            opts="-v -i -h --verbose --interactive --cmd --bash --pwsh --zsh --sh --fish --host --http --timeout --ip --ac-id --minimized --visible --state-dir --machine --admin-error --signal-only --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --host)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --timeout)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ip)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ac-id)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --state-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --admin-error)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__logout)
            opts="-h --host --http --timeout --ip --ac-id --minimized --visible --state-dir --machine --admin-error --signal-only --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --host)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --timeout)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ip)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ac-id)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --state-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --admin-error)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__quit)
            opts="-h --timeout --all-sessions --host --http --ip --ac-id --minimized --visible --state-dir --machine --admin-error --signal-only --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --timeout)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --host)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ip)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ac-id)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --state-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --admin-error)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__selftest)
            opts="-h --host --http --timeout --ip --ac-id --minimized --visible --state-dir --machine --admin-error --signal-only --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --host)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --timeout)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ip)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ac-id)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --state-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --admin-error)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__service)
            opts="-h --host --http --timeout --ip --ac-id --minimized --visible --state-dir --machine --admin-error --signal-only --help install uninstall enable disable start stop restart status"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --host)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --timeout)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ip)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ac-id)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --state-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --admin-error)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__service__subcmd__admin)
            opts="-h --host --http --timeout --ip --ac-id --minimized --visible --state-dir --machine --admin-error --signal-only --help install uninstall enable disable start stop restart status"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --host)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --timeout)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ip)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ac-id)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --state-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --admin-error)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__service__subcmd__purge)
            opts="-h --host --http --timeout --ip --ac-id --minimized --visible --state-dir --machine --admin-error --signal-only --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --host)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --timeout)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ip)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ac-id)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --state-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --admin-error)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__service__subcmd__restore)
            opts="-h --host --http --timeout --ip --ac-id --minimized --visible --state-dir --machine --admin-error --signal-only --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --host)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --timeout)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ip)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ac-id)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --state-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --admin-error)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__shortcut)
            opts="-h --host --http --timeout --ip --ac-id --minimized --visible --state-dir --machine --admin-error --signal-only --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --host)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --timeout)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ip)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ac-id)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --state-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --admin-error)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
        thunet__subcmd__status)
            opts="-h --host --http --timeout --ip --ac-id --minimized --visible --state-dir --machine --admin-error --signal-only --help"
            if [[ ${cur} == -* || ${COMP_CWORD} -eq 2 ]] ; then
                COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
                return 0
            fi
            case "${prev}" in
                --host)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --timeout)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ip)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --ac-id)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --state-dir)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                --admin-error)
                    COMPREPLY=($(compgen -f "${cur}"))
                    return 0
                    ;;
                *)
                    COMPREPLY=()
                    ;;
            esac
            COMPREPLY=( $(compgen -W "${opts}" -- "${cur}") )
            return 0
            ;;
    esac
}

if [[ "${BASH_VERSINFO[0]}" -eq 4 && "${BASH_VERSINFO[1]}" -ge 4 || "${BASH_VERSINFO[0]}" -gt 4 ]]; then
    complete -F _thunet -o nosort -o bashdefault -o default thunet
else
    complete -F _thunet -o bashdefault -o default thunet
fi
