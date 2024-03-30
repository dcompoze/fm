_fm() {
    local -a options
    local -a file_path

    options=(
        '--help:Show help information'
        '--config[Specify config file location]:file_path:_files'
    )

    _arguments $options
}

compdef _fm fm
