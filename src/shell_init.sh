# claustre shell integration
# Add to your .zshrc or .bashrc:
#   eval "$(claustre shell-init)"

claustre() {
    if [ "$1" = "sync" ] && [ "$2" = "cd" ] && [ $# -eq 2 ]; then
        local dir
        dir=$(command claustre sync cd)
        if [ -n "$dir" ] && [ -d "$dir" ]; then
            cd "$dir" || return
        else
            echo "claustre: sync directory not found — run 'claustre sync init' first" >&2
            return 1
        fi
    else
        command claustre "$@"
    fi
}
