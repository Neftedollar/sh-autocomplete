[[ -n "${SHAC_DISABLE:-}" ]] && return 0 2>/dev/null; or return

if status is-interactive
  if not set -q _SHAC_FISH_LOADED
    set -g _SHAC_FISH_LOADED 1

    set -g _shac_last_request_id ""
    set -g _shac_last_accepted_item_key ""

    # Returns completions for the current commandline in fish's tab-completion format.
    function __shac_complete
      set -l line (commandline)
      set -l cursor (commandline --cursor)
      set -l tty_value (tty 2>/dev/null; or echo "")
      set -l -a history_args
      for hist_line in (builtin history | head -10)
        switch $hist_line
          case 'shac *' '_shac_*'
            continue
        end
        set -a history_args --history-command $hist_line
      end
      TTY=$tty_value shac complete \
        --shell fish \
        --line $line \
        --cursor $cursor \
        --cwd $PWD \
        $history_args \
        --format shell-tsv-v2 \
        2>/dev/null | while read -l item_line
        if string match -q '__shac_request_id*' -- $item_line
          set -g _shac_last_request_id (string split \t -- $item_line)[2]
          continue
        end
        test -z "$item_line"; and continue
        set -l fields (string split \t -- $item_line)
        test (count $fields) -lt 2; and continue
        set -g _shac_last_accepted_item_key $fields[1]
        set -l insert_text $fields[2]
        set -l description ""
        test (count $fields) -ge 6; and set description $fields[6]
        if test -n "$description"
          printf '%s\t%s\n' $insert_text $description
        else
          printf '%s\n' $insert_text
        end
      end
    end

    # Ctrl+F: accepts the top shac suggestion inline, like zsh's ghost-text accept.
    function __shac_accept_suggestion
      set -l line (commandline)
      if test -z "$line"
        commandline -f forward-char
        return
      end
      set -l cursor (commandline --cursor)
      set -l tty_value (tty 2>/dev/null; or echo "")
      set -l response (TTY=$tty_value shac complete \
        --shell fish \
        --line $line \
        --cursor $cursor \
        --cwd $PWD \
        --format shell-tsv-v2 \
        2>/dev/null | head -n2)
      set -l insert_text ""
      set -l item_key ""
      set -l request_id ""
      for item_line in $response
        if string match -q '__shac_request_id*' -- $item_line
          set request_id (string split \t -- $item_line)[2]
        else if test -n "$item_line"
          set -l fields (string split \t -- $item_line)
          if test (count $fields) -ge 2
            set item_key $fields[1]
            set insert_text $fields[2]
          end
        end
      end
      if test -n "$insert_text"
        commandline -t -- $insert_text
        commandline -f end-of-line
        set -g _shac_last_request_id $request_id
        set -g _shac_last_accepted_item_key $item_key
      else
        commandline -f forward-char
      end
    end

    function __shac_record --on-event fish_preexec
      set -l cmd $argv[1]
      switch $cmd
        case 'shac *' '_shac_*' ''
          return
      end
      set -l -a record_args \
        --shell fish \
        --cwd $PWD \
        --command $cmd \
        --trust interactive \
        --provenance typed_manual \
        --origin fish_preexec \
        --tty-present
      test -n "$_shac_last_request_id"; \
        and set -a record_args --accepted-request-id $_shac_last_request_id
      test -n "$_shac_last_accepted_item_key"; \
        and set -a record_args --accepted-item-key $_shac_last_accepted_item_key
      shac record-command $record_args >/dev/null 2>&1
      set -g _shac_last_request_id ""
      set -g _shac_last_accepted_item_key ""
    end

    # Fallback completions for commands without specific fish completions registered.
    complete --command '*' --arguments '(__shac_complete)' 2>/dev/null; or true

    # Ctrl+F accepts the top suggestion (mirrors zsh inline ghost-text accept).
    bind \cf __shac_accept_suggestion
    bind -M insert \cf __shac_accept_suggestion 2>/dev/null; or true
  end
end
